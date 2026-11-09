use super::*;
use crate::{IntentType, Priority};
use std::num::NonZeroU32;

fn work_events(f: &Fixture) -> Vec<&'static str> {
    f.device
        .events
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|e| !matches!(*e, "push" | "pop"))
        .collect()
}

#[test]
fn capped_grids_cover_every_descriptor_vector_and_byte_tail_both_directions() {
    for (n, width) in [(257usize, 5usize), (3, 131_073)] {
        let stride = width.div_ceil(16) * 16 + 32;
        let size = (n + 2) * stride;
        let f = Fixture::sized(local_driver(), size);
        for cap in [
            None,
            NonZeroU32::new(1),
            NonZeroU32::new(2),
            NonZeroU32::new(256),
        ] {
            let mut config = config(Kernel);
            config.kernel_cta_limit = cap;
            let worker = f.bind(config).unwrap();
            for dir in [HostToDevice, DeviceToHost] {
                // Alternating alignment covers vector and byte loops; widths include tails.
                let rows: Vec<_> = (0..n)
                    .map(|i| {
                        let off = (i + 1) * stride + i % 2;
                        f.desc(off, off, width)
                    })
                    .collect();
                let source: Vec<_> = (0..size).map(|i| (i % 251) as u8).collect();
                let (src, dst) = if dir == HostToDevice {
                    (&f.host, &f.gpu)
                } else {
                    (&f.gpu, &f.host)
                };
                src.write(0, &source).unwrap();
                dst.write(0, &vec![GUARD; size]).unwrap();
                let receipt = worker
                    .submit(dir, &rows, LocalCopyOptions::default())
                    .unwrap();
                receipt.wait(EVENTUAL_PROGRESS).unwrap();
                let budget = cap.map_or(64, |v| 64.min(v.get() as usize));
                let ctas = (budget / n)
                    .clamp(1, 16)
                    .min(width.div_ceil(65536).clamp(1, 16));
                let grid = (n * ctas).min(cap.map_or(65535, |v| v.get() as usize));
                assert_eq!(
                    *f.device.local_launches.lock().unwrap().last().unwrap(),
                    (grid as u32, n, ctas)
                );
                let mut expected = vec![GUARD; size];
                for i in 0..n {
                    let off = (i + 1) * stride + i % 2;
                    expected[off..off + width].copy_from_slice(&source[off..off + width]);
                }
                let mut actual = vec![0; size];
                dst.read(0, &mut actual).unwrap();
                assert_eq!(actual, expected);
                src.read(0, &mut actual).unwrap();
                assert_eq!(actual, source);
                let events = f.device.events.lock().unwrap().clone();
                let stats = worker.stats().unwrap();
                assert_eq!(
                    *f.device.events.lock().unwrap(),
                    events,
                    "stats must never call CUDA"
                );
                let class = stats.classes.iter().find(|c| c.direction == dir).unwrap();
                assert_eq!(
                    (class.last_kernel_ctas, class.kernel_ctas),
                    (grid as u32, grid as u64)
                );
                assert_eq!(
                    (
                        class.kernel_submissions,
                        class.dma_submissions,
                        class.last_fallback
                    ),
                    (1, 0, None)
                );
            }
            worker.close(EVENTUAL_PROGRESS).unwrap();
        }
    }
}

#[test]
fn auto_selection_matrix_keeps_stream_order_and_reports_each_submission_class() {
    use IntentType::*;
    use LocalCopyFallback::*;
    let intents = [
        Unspec,
        ForegroundGet,
        BackgroundPrefetch,
        Migration,
        Checkpoint,
        WeightLoading,
        StagingInternal,
    ];
    for image in [true, false] {
        let mut api = local_driver();
        if !image {
            api.local.kernel = None;
        }
        let f = Fixture::new(api);
        for backend in [Auto, Dma, Kernel] {
            let mut config = config(backend);
            config.kernel_threshold_background = 6;
            let result = f.bind(config);
            if !image && backend == Kernel {
                assert!(matches!(result, Err(Error::Unavailable(_))));
                continue;
            }
            let worker = result.unwrap();
            for dir in [HostToDevice, DeviceToHost] {
                for intent in intents {
                    for priority in [Priority::High, Priority::Medium, Priority::Low] {
                        for n in [0, 2, 3, 5, 6] {
                            for overlap in [false, true] {
                                let rows: Vec<_> = (0..n)
                                    .map(|i| {
                                        let (d, h) = (32 + i * 16, 64 + i * 16);
                                        (
                                            if overlap && dir == HostToDevice {
                                                32
                                            } else {
                                                d
                                            },
                                            if overlap && dir == DeviceToHost {
                                                64
                                            } else {
                                                h
                                            },
                                            1 + i % 2,
                                        )
                                    })
                                    .collect();
                                let descs: Vec<_> =
                                    rows.iter().map(|&(d, h, n)| f.desc(d, h, n)).collect();
                                let options = LocalCopyOptions {
                                    intent,
                                    priority,
                                    ..Default::default()
                                };
                                if overlap && n > 0 && backend == Kernel {
                                    let before = work_events(&f);
                                    let counts = worker.stats().unwrap();
                                    assert!(matches!(
                                        worker.submit(dir, &descs, options),
                                        Err(Error::InvalidConfig(_))
                                    ));
                                    assert_eq!(worker.stats().unwrap(), counts);
                                    assert_eq!(work_events(&f), before);
                                    continue;
                                }
                                let background = dir == DeviceToHost
                                    && matches!(
                                        intent,
                                        BackgroundPrefetch | Migration | Checkpoint
                                    );
                                let reason = if backend != Auto {
                                    None
                                } else if n == 0 {
                                    Some(Threshold)
                                } else if overlap {
                                    Some(Overlap)
                                } else if n < 3 {
                                    Some(Threshold)
                                } else if background && n < 6 {
                                    Some(Intent)
                                } else if !image {
                                    Some(ImageUnavailable)
                                } else {
                                    None
                                };
                                let chosen = if n != 0 && backend != Dma && reason.is_none() {
                                    Kernel
                                } else {
                                    Dma
                                };
                                f.guarded(dir, chosen, &rows, || {
                                    worker.submit(dir, &descs, options)
                                });
                                let stats = worker.stats().unwrap();
                                let c = stats
                                    .classes
                                    .iter()
                                    .find(|c| {
                                        c.direction == dir
                                            && c.intent == intent
                                            && c.priority == priority
                                    })
                                    .unwrap();
                                assert_eq!((c.last_backend, c.last_fallback), (chosen, reason));
                                assert_eq!(
                                    c.last_kernel_ctas,
                                    if chosen == Kernel { n as u32 } else { 0 }
                                );
                            }
                        }
                    }
                }
            }
            assert_eq!(worker.stats().unwrap().classes.len(), 42);
            worker.close(EVENTUAL_PROGRESS).unwrap();
        }
    }
}

#[test]
fn invalid_cap_and_background_threshold_fail_before_any_partial_initialization() {
    let f = Fixture::new(local_driver());
    f.device.local_max_grid.store(32, Ordering::Release);
    for backend in [Auto, Dma, Kernel] {
        for (limit, threshold) in [(33, 6), (65536, 6), (1, 0), (1, 3)] {
            let mut config = config(backend);
            config.kernel_cta_limit = NonZeroU32::new(limit);
            config.kernel_threshold_background = threshold;
            let events = work_events(&f);
            let refs = Arc::strong_count(&f.stream);
            assert!(matches!(f.bind(config), Err(Error::InvalidConfig(_))));
            assert_eq!(work_events(&f), events);
            assert_eq!(Arc::strong_count(&f.stream), refs);
            assert!(f.device.scratch.lock().unwrap().is_empty());
            assert_eq!(f.device.pins.lock().unwrap().len(), 1);
            assert!(f.device.completions.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn default_options_and_no_cap_preserve_the_original_threshold_and_grid() {
    let f = Fixture::new(local_driver());
    let config = LocalCopyConfig::default();
    assert!(config.kernel_cta_limit.is_none());
    assert_eq!(config.kernel_threshold, 128);
    let worker = f.bind(config).unwrap();
    for dir in [HostToDevice, DeviceToHost] {
        for n in [127, 128, 129] {
            let rows: Vec<_> = (0..n)
                .map(|i| (8 + 16 * i, 8 + 16 * i, 1 + i % 2))
                .collect();
            let descs: Vec<_> = rows.iter().map(|&(d, h, n)| f.desc(d, h, n)).collect();
            let backend = if n < 128 { Dma } else { Kernel };
            f.guarded(dir, backend, &rows, || {
                worker.submit(dir, &descs, LocalCopyOptions::default())
            });
            if backend == Kernel {
                assert_eq!(
                    *f.device.local_launches.lock().unwrap().last().unwrap(),
                    (n as u32, n, 1)
                );
            }
        }
    }
}

#[test]
fn a_failed_descriptor_upload_does_not_count_a_kernel_launch_or_release_early() {
    let f = Fixture::new(local_driver());
    let worker = f.bind(config(Kernel)).unwrap();
    f.device.copy_error.store(700, Ordering::Release);
    f.device.query_hold.store(true, Ordering::Release);
    let flight = worker
        .submit(
            HostToDevice,
            &[f.desc(0, 0, 8)],
            LocalCopyOptions::default(),
        )
        .unwrap();
    assert!(!flight.poll().unwrap());
    assert!(f.host.write(0, b"x").is_err());
    let stats = worker.stats().unwrap();
    assert_eq!(
        (stats.active, stats.payload_bytes, stats.classes.len()),
        (1, 8, 1)
    );
    assert_eq!(stats.classes[0].kernel_submissions, 1);
    assert_eq!(
        (
            stats.classes[0].kernel_ctas,
            stats.classes[0].last_kernel_ctas
        ),
        (0, 0)
    );
    assert!(f.device.local_launches.lock().unwrap().is_empty());
    f.device.query_hold.store(false, Ordering::Release);
    assert!(matches!(
        flight.poll(),
        Err(Error::Backend { code: 700, .. })
    ));
    assert_eq!(worker.stats().unwrap().active, 0);
    f.host.write(0, b"x").unwrap();
    worker.close(EVENTUAL_PROGRESS).unwrap();
}
