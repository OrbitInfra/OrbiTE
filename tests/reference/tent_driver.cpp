// Test-only adapter to the frozen TENT C ABI. No Mooncake code is vendored.
#include <algorithm>
#include <array>
#include <charconv>
#include <chrono>
#include <cstdlib>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <memory>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

#include "tent/transfer_engine.h"

namespace {
void require(bool condition, const std::string &message) {
  if (!condition)
    throw std::runtime_error(message);
}

void checked(int result, const std::string &operation) {
  require(result == 0, "TENT failed: " + operation);
}

size_t number(const std::string &value) {
  size_t result = 0;
  auto [end, error] =
      std::from_chars(value.data(), value.data() + value.size(), result);
  require(error == std::errc{} && end == value.data() + value.size(),
          "invalid integer");
  return result;
}

std::vector<std::string> split(const std::string &value, char separator) {
  std::istringstream input(value);
  std::vector<std::string> result;
  for (std::string field; std::getline(input, field, separator);)
    result.push_back(field);
  return result;
}

std::string text(const std::string &value) { return value == "-" ? "" : value; }

std::string hex(const void *data, size_t length) {
  if (length == 0)
    return "-";
  std::ostringstream result;
  const auto *bytes = static_cast<const unsigned char *>(data);
  for (size_t i = 0; i < length; ++i)
    result << std::hex << std::setfill('0') << std::setw(2)
           << unsigned(bytes[i]);
  return result.str();
}

void range(size_t offset, size_t length, size_t capacity) {
  require(length > 0 && offset <= capacity && length <= capacity - offset,
          "invalid buffer range");
}

using Engine = std::unique_ptr<void, decltype(&tent_destroy_engine)>;

Engine engine() {
  Engine result(tent_create_engine(), tent_destroy_engine);
  require(result && tent_available(result.get()) > 0,
          "TENT engine unavailable");
  return result;
}

tent_batch_id_t allocate_batch(tent_engine_t engine, size_t capacity) {
  const auto batch = tent_allocate_batch(engine, capacity);
  require(batch != 0, "batch allocation failed");
  return batch;
}

void finish_batch(tent_engine_t engine, tent_batch_id_t batch,
                  size_t task_count, const std::string &label) {
  tent_status_t status{};
  const auto end = std::chrono::steady_clock::now() + std::chrono::seconds(5);
  do {
    checked(tent_overall_status(engine, batch, &status), "batch status");
    if (status.status == STATUS_COMPLETED)
      break;
    require(status.status == STATUS_PENDING || status.status == STATUS_WAITING,
            "transfer reached failed terminal state");
    require(std::chrono::steady_clock::now() < end,
            "transfer deadline expired");
    std::this_thread::sleep_for(std::chrono::microseconds(100));
  } while (true);
  for (int i = 0; i < 2; ++i) {
    tent_status_t repeated{};
    checked(tent_overall_status(engine, batch, &repeated), "repeat status");
    require(repeated.status == status.status &&
                repeated.transferred_bytes == status.transferred_bytes,
            "terminal status changed");
  }
  // The C wrapper truncates to the supplied capacity. Leave one extra slot
  // so a surplus task cannot look like the requested cardinality.
  std::vector<tent_status_t> tasks(task_count + 1);
  size_t reported = tasks.size();
  checked(tent_task_status_list(engine, batch, tasks.data(), &reported),
          "task statuses");
  require(reported == task_count, "unexpected task count");
  auto remaining = status.transferred_bytes;
  for (size_t i = 0; i < reported; ++i) {
    require(tasks[i].status == STATUS_COMPLETED, "task did not complete");
    require(tasks[i].transferred_bytes <= remaining, "task bytes exceed batch");
    remaining -= tasks[i].transferred_bytes;
  }
  require(remaining == 0, "task bytes do not cover batch");
  std::cout << "OBS\tRESULT\t" << label << "\tcompleted\t" << reported << '\t'
            << status.transferred_bytes << '\n';
  checked(tent_free_batch(engine, batch), "free batch");
}

struct Buffer {
  std::vector<unsigned char> bytes;
  size_t references = 0;
};

void command(const std::string &expected) {
  std::string line;
  require(std::getline(std::cin, line) && line == expected,
          "expected controller command: " + expected);
}

[[noreturn]] void stop_trace(const std::exception &error) {
  // A failed submit or poll is not a receipt for destroying active backing.
  std::cerr << error.what() << std::endl;
  std::_Exit(EXIT_FAILURE);
}

void pending_source(std::string payload) {
  auto source = engine();
  try {
    checked(tent_register_memory_with_perm(source.get(), payload.data(),
                                           payload.size(),
                                           PERM_GLOBAL_READ_ONLY),
            "register pending-read source");
    char name[256]{};
    checked(tent_segment_name(source.get(), name, sizeof(name)),
            "segment name");
    std::cout << "READY " << name << std::endl;
    command("close");
    checked(
        tent_unregister_memory(source.get(), payload.data(), payload.size()),
        "unregister pending-read source");
  } catch (const std::exception &error) {
    stop_trace(error);
  }
}

void pending_reads(const char *slow, const char *fast) {
  std::array<std::array<unsigned char, 8>, 3> dst;
  auto requester = engine();
  auto *te = requester.get();
  try {
    const std::array names{slow, fast};
    const std::array<size_t, 3> lengths{5, 3, 2};
    std::array<tent_request_t, 3> req{};
    for (size_t i = 0; i < req.size(); ++i) {
      dst[i].fill(0xee);
      checked(tent_register_memory_with_perm(te, dst[i].data(), dst[i].size(),
                                             PERM_LOCAL_READ_WRITE),
              "register destination");
      req[i].opcode = OPCODE_READ;
      req[i].transport_hint = TRANSPORT_TCP;
      req[i].source = dst[i].data() + 1;
      req[i].length = lengths[i];
    }
    for (size_t i = 0; i < names.size(); ++i) {
      checked(tent_open_segment(te, &req[i].target_id, names[i]), "open peer");
    }
    require(req[0].target_id != req[1].target_id, "expected two source peers");
    // Opening a segment invalidates the submit thread's descriptor cache.
    // Resolve both addresses after both opens, while both peers can reply.
    for (size_t i = 0; i < names.size(); ++i) {
      tent_segment_info_t info{};
      checked(tent_get_segment_info(te, req[i].target_id, &info),
              "describe peer");
      require(info.num_buffers == 1 && info.buffers &&
                  info.buffers[0].length == 8,
              "expected one eight-byte source");
      req[i].target_offset = info.buffers[0].base;
      tent_free_segment_info(&info);
    }
    req[2].target_id = req[0].target_id;
    req[2].target_offset = req[0].target_offset + 3;
    auto submit = [&](size_t i) {
      auto id = allocate_batch(te, 1);
      checked(tent_submit(te, id, &req[i], 1), "submit pending-read case");
      return id;
    };
    auto finish = [&](size_t i, tent_batch_id_t id, const char *label) {
      finish_batch(te, id, 1, label);
      // Frozen task lookup checks liveness before accessing the batch.
      // No intervening allocation can reuse this just-freed ID.
      tent_status_t retired{};
      require(tent_task_status(te, id, 0, &retired) != 0,
              "freed batch still exposes its task");
      std::cout << "OBS\tBYTES\t" << label << '\t'
                << hex(dst[i].data(), dst[i].size()) << std::endl;
    };
    std::cout << "OPEN" << std::endl;
    command("stopped");
    auto first = submit(0);
    auto check_pending = [&](tent_batch_id_t id) {
      tent_status_t task{}, batch{};
      checked(tent_task_status(te, id, 0, &task), "pending task status");
      checked(tent_overall_status(te, id, &batch), "pending batch status");
      for (auto status : {task, batch})
        require((status.status == STATUS_WAITING ||
                 status.status == STATUS_PENDING) &&
                    status.transferred_bytes == 0,
                "READ no longer pending");
    };
    check_pending(first);
    auto second = submit(1);
    require(second != first, "live batches share an ID");
    finish(1, second, "second");
    check_pending(first);
    std::cout << "PENDING first" << std::endl;
    auto later = submit(2);
    require(later != first, "same-peer batches share an ID");
    check_pending(first);
    check_pending(later);
    std::cout << "PENDING first later" << std::endl;
    command("resumed");
    finish(2, later, "later");
    finish(0, first, "first");
    for (auto &buffer : dst) {
      checked(tent_unregister_memory(te, buffer.data(), buffer.size()),
              "unregister destination");
    }
    for (size_t i = 0; i < names.size(); ++i) {
      checked(tent_close_segment(te, req[i].target_id), "close peer");
    }
  } catch (const std::exception &error) {
    stop_trace(error);
  }
}

class Trace {
  // Backing outlives both engines during normal teardown.
  std::map<std::string, Buffer> buffers_;
  Engine source_{engine()};
  Engine requester_{engine()};
  std::optional<tent_segment_id_t> peer_;
  tent_notifi_info poll_{};

  void *owner(const std::string &name) {
    require(name == "source" || name == "local", "unknown buffer owner");
    return name == "source" ? source_.get() : requester_.get();
  }

  void describe(const std::string &label) {
    if (peer_)
      checked(tent_close_segment(requester_.get(), *peer_), "close segment");
    char name[256]{};
    checked(tent_segment_name(source_.get(), name, sizeof(name)),
            "segment name");
    tent_segment_id_t peer = 0;
    checked(tent_open_segment(requester_.get(), &peer, name), "open segment");
    require(peer != LOCAL_SEGMENT_ID, "remote peer resolved to self");
    peer_ = peer;
    tent_segment_info_t info{};
    checked(tent_get_segment_info(requester_.get(), peer, &info), "describe");
    require(info.num_buffers >= 0 && info.num_buffers <= 1,
            "unexpected descriptor count");
    size_t length = 0;
    for (int i = 0; i < info.num_buffers; ++i) {
      require(info.buffers != nullptr, "missing descriptor storage");
      const auto &buffer = buffers_.at("source").bytes;
      require(info.buffers[i].base ==
                      reinterpret_cast<uint64_t>(buffer.data()) &&
                  info.buffers[i].length == buffer.size(),
              "descriptor does not identify actual backing");
      length += info.buffers[i].length;
    }
    std::cout << "OBS\tDESCRIBE\t" << label << '\t' << info.num_buffers << '\t'
              << length << '\n';
    tent_free_segment_info(&info);
  }

  void unregister_buffer(const std::string &name) {
    auto &buffer = buffers_.at(name);
    require(buffer.references > 0, "buffer not registered");
    checked(tent_unregister_memory(owner(name), buffer.bytes.data(),
                                   buffer.bytes.size()),
            "unregister");
    --buffer.references;
  }

  void transfer(const std::vector<std::string> &fields) {
    require(peer_.has_value(), "DESCRIBE must precede transfers");
    auto slices =
        fields[2] == "-" ? std::vector<std::string>{} : split(fields[2], ',');
    std::vector<tent_request_t> entries(std::max<size_t>(1, slices.size()));
    for (size_t i = 0; i < slices.size(); ++i) {
      auto parts = split(slices[i], ':');
      require(parts.size() == 3, "invalid slice");
      const auto local = number(parts[0]), remote = number(parts[1]),
                 length = number(parts[2]);
      auto &source = buffers_.at("source").bytes;
      auto &destination = buffers_.at("local").bytes;
      range(local, length, destination.size());
      range(remote, length, source.size());
      entries[i].opcode = fields[0] == "WRITE" ? OPCODE_WRITE : OPCODE_READ;
      entries[i].source = destination.data() + local;
      entries[i].target_id = *peer_;
      entries[i].target_offset =
          reinterpret_cast<uint64_t>(source.data() + remote);
      entries[i].length = length;
      entries[i].transport_hint = TRANSPORT_TCP;
    }
    if (fields[0] == "READ_PAIR_UNREGISTER") {
      require(slices.size() == 2 && fields[3] == "~" &&
                  buffers_.at("source").references > 1,
              "paired read needs two requests, no notification and a "
              "registration alias");
      const auto *a = static_cast<unsigned char *>(entries[0].source);
      const auto *b = static_cast<unsigned char *>(entries[1].source);
      require(a + entries[0].length <= b || b + entries[1].length <= a,
              "paired read destinations overlap");
      const auto first = allocate_batch(requester_.get(), 1);
      checked(tent_submit(requester_.get(), first, &entries[0], 1),
              "first submit");
      const auto second = allocate_batch(requester_.get(), 1);
      require(first != second, "live batches share an ID");
      checked(tent_submit(requester_.get(), second, &entries[1], 1),
              "second submit");
      unregister_buffer("source");
      std::cout << "OBS\tLIVE\t" << fields[1] << "\t2\n";
      finish_batch(requester_.get(), second, 1, fields[1] + "-second");
      finish_batch(requester_.get(), first, 1, fields[1] + "-first");
      return;
    }
    const auto batch = allocate_batch(requester_.get(), entries.size());
    auto name = text(fields[3]), message = text(fields[4]);
    if (fields[0] == "READ_SPLIT") {
      require(fields[3] == "~" && !slices.empty(), "invalid split submission");
      for (size_t i = 0; i < slices.size(); ++i)
        checked(tent_submit(requester_.get(), batch, &entries[i], 1),
                "append submit");
    } else if (fields[3] == "~")
      checked(
          tent_submit(requester_.get(), batch, entries.data(), slices.size()),
          "submit");
    else
      checked(tent_submit_notif(requester_.get(), batch, entries.data(),
                                slices.size(), name.c_str(), message.c_str()),
              "submit notification");
    finish_batch(requester_.get(), batch, slices.size(), fields[1]);
  }

  void take(const std::string &label) {
    checked(tent_recv_notifs(source_.get(), &poll_), "receive notifications");
    require(poll_.num_records >= 0 &&
                (poll_.num_records == 0 ? poll_.records == nullptr
                                        : poll_.records != nullptr),
            "invalid notification output");
    std::cout << "OBS\tTAKE\t" << label << '\t' << poll_.num_records << '\t';
    for (int i = 0; i < poll_.num_records; ++i) {
      if (i)
        std::cout << ',';
      const auto &record = poll_.records[i];
      std::string name(record.name), message(record.msg);
      std::cout << hex(name.data(), name.size()) << ':'
                << hex(message.data(), message.size());
    }
    if (!poll_.num_records)
      std::cout << '-';
    std::cout << '\n';
    tent_free_notifs(&poll_);
    require(poll_.num_records == 0 && poll_.records == nullptr,
            "notification free did not reset output");
    tent_free_notifs(&poll_);
  }

public:
  ~Trace() { tent_free_notifs(&poll_); }

  void execute(const std::vector<std::string> &fields) {
    const auto &operation = fields.at(0);
    if (operation == "ORBITE_CPU_TRACE" && fields.size() == 2 &&
        fields[1] == "1") {
      std::cout << "OBS\tVERSION\t1\n";
    } else if (operation == "BUFFER" && fields.size() == 4) {
      const auto length = number(fields[2]);
      require(length > 0 && length <= (1 << 20) &&
                  !buffers_.contains(fields[1]),
              "invalid buffer");
      owner(fields[1]);
      Buffer buffer{std::vector<unsigned char>(length)};
      const auto initial = fields[3] == "pattern" ? 0 : number(fields[3]);
      require(initial <= 255, "invalid fill byte");
      for (size_t i = 0; i < length; ++i)
        buffer.bytes[i] = fields[3] == "pattern" ? i % 251 : initial;
      buffers_.emplace(fields[1], std::move(buffer));
    } else if (operation == "REGISTER" && fields.size() == 3) {
      auto &buffer = buffers_.at(fields[1]);
      int permission;
      if (fields[2] == "rw")
        permission = PERM_GLOBAL_READ_WRITE;
      else if (fields[2] == "r")
        permission = PERM_GLOBAL_READ_ONLY;
      else {
        require(fields[2] == "local", "unknown permission");
        permission = PERM_LOCAL_READ_WRITE;
      }
      checked(tent_register_memory_with_perm(owner(fields[1]),
                                             buffer.bytes.data(),
                                             buffer.bytes.size(), permission),
              "register");
      ++buffer.references;
    } else if (operation == "REGISTER_BATCH" && fields.size() == 4) {
      auto &buffer = buffers_.at(fields[1]);
      const auto count = number(fields[3]);
      require(fields[2] == "r" && count > 0 && count <= 1024 &&
                  buffer.references > 0,
              "invalid registration batch");
      std::vector<void *> addresses(count, buffer.bytes.data());
      std::vector<size_t> lengths(count, buffer.bytes.size());
      checked(tent_register_memory_batch(owner(fields[1]), addresses.data(),
                                         lengths.data(), count,
                                         PERM_GLOBAL_READ_ONLY),
              "register batch");
      buffer.references += count;
      std::cout << "OBS\tREGISTER_BATCH\t" << fields[1] << '\t' << count
                << '\n';
    } else if (operation == "UNREGISTER" && fields.size() == 2) {
      unregister_buffer(fields[1]);
    } else if (operation == "DESCRIBE" && fields.size() == 2) {
      describe(fields[1]);
    } else if (operation == "FILL" && fields.size() == 5) {
      auto &buffer = buffers_.at(fields[1]).bytes;
      const auto offset = number(fields[2]), length = number(fields[3]),
                 value = number(fields[4]);
      range(offset, length, buffer.size());
      require(value <= 255, "invalid fill byte");
      std::fill_n(buffer.begin() + offset, length, value);
    } else if (operation == "BYTES" && fields.size() == 5) {
      const auto &buffer = buffers_.at(fields[2]).bytes;
      const auto offset = number(fields[3]), length = number(fields[4]);
      range(offset, length, buffer.size());
      std::cout << "OBS\tBYTES\t" << fields[1] << '\t'
                << hex(buffer.data() + offset, length) << '\n';
    } else if ((operation == "READ" || operation == "WRITE" ||
                operation == "READ_SPLIT" ||
                operation == "READ_PAIR_UNREGISTER") &&
               fields.size() == 5) {
      transfer(fields);
    } else if (operation == "NOTIFY" && fields.size() == 5) {
      require(fields[2] == "local" || (fields[2] == "remote" && peer_),
              "unknown notification route");
      auto name = text(fields[3]), message = text(fields[4]);
      checked(tent_send_notifs(fields[2] == "local" ? source_.get()
                                                    : requester_.get(),
                               fields[2] == "local" ? LOCAL_SEGMENT_ID : *peer_,
                               name.c_str(), message.c_str()),
              "send notification");
      std::cout << "OBS\tNOTIFY\t" << fields[1] << "\tok\n";
    } else if (operation == "TAKE" && fields.size() == 2) {
      take(fields[1]);
    } else {
      throw std::runtime_error("invalid trace instruction");
    }
  }

  void finish() {
    for (const auto &[name, buffer] : buffers_)
      require(buffer.references == 0,
              "trace did not unregister all references");
    if (peer_)
      checked(tent_close_segment(requester_.get(), *peer_),
              "close final segment");
    requester_.reset();
    source_.reset();
    std::cout << "OBS\tCLEANUP\tok\n";
  }
};

// The initiator exits between phases. Inspect and change the registered target
// only after its completed WRITE or READ, with no other requester admitted.
void tebench_target() {
  constexpr size_t guard = 32, payload = 2 * 4096;
  // The frozen address-derived seed is always nonzero, including for the
  // address chosen by ASLR. A seeded WRITE into either zero guard is
  // observable.
  std::vector<unsigned char> bytes(guard + payload + guard, 0);
  auto first = bytes.begin() + guard, last = first + payload;
  auto source = engine();
  checked(tent_register_memory_with_perm(source.get(), bytes.data(),
                                         bytes.size(), PERM_GLOBAL_READ_WRITE),
          "register tebench target");
  char name[256]{};
  checked(tent_segment_name(source.get(), name, sizeof(name)),
          "tebench segment name");
  std::cout << "READY " << name << std::endl;

  auto check_guards = [&] {
    auto intact = [](unsigned char byte) { return byte == 0; };
    require(std::all_of(bytes.begin(), first, intact) &&
                std::all_of(last, bytes.end(), intact),
            "tebench changed target guard bytes");
  };
  command("flip");
  check_guards();
  // Frozen write_seed fills DRAM/TCP payloads with one nonzero address-derived
  // byte.
  const auto seed = *first;
  auto seeded = [seed](unsigned char byte) { return byte == seed; };
  require(seed != 0 && std::all_of(first, last, seeded),
          "tebench WRITE left incorrect bytes");
  *(last - 1) ^= 1;
  std::cout << "FLIPPED " << payload - 1 << std::endl;

  command("restore");
  check_guards();
  require(std::all_of(first, last - 1, seeded) && *(last - 1) == (seed ^ 1),
          "tebench READ changed the target");
  *(last - 1) = seed;
  std::cout << "RESTORED" << std::endl;
  command("close");
  check_guards();
  require(std::all_of(first, last, seeded),
          "tebench READ changed the restored target");
  checked(tent_unregister_memory(source.get(), bytes.data(), bytes.size()),
          "unregister tebench target");
  source.reset();
  std::cout << "CLOSED" << std::endl;
}
} // namespace

int main(int argc, char **argv) {
  try {
    require(argc >= 2, "expected trace path or driver role");
    tent_set_config("metadata_type", "p2p");
    tent_set_config("rpc_server_hostname", "127.0.0.1");
    tent_set_config("rpc_server_port", "0");
    tent_set_config("rpc_server_threads", "4");
    tent_set_config("log_level", "error");
    tent_set_config("enable_progress_worker", "false");
    tent_set_config("enable_runtime_queue", "false");
    tent_set_config("merge_requests", "false");
    for (const auto *transport :
         {"tcp", "rdma", "shm", "hp_tcp", "io_uring", "nvlink", "mnnvl", "gds",
          "ascend_direct", "sunrise_link", "mpcomm", "tpu", "ub", "xpu",
          "hylink", "fabric", "mlu", "memfabric"}) {
      const auto key = std::string("transports/") + transport + "/enable";
      tent_set_config(key.c_str(),
                      std::string(transport) == "tcp" ? "true" : "false");
    }
    if (argc == 3 && std::string(argv[1]) == "--pending-source") {
      pending_source(argv[2]);
      return 0;
    }
    if (argc == 4 && std::string(argv[1]) == "--pending-reads") {
      pending_reads(argv[2], argv[3]);
      return 0;
    }
    require(argc == 2, "expected trace path or --tebench-target");
    if (std::string(argv[1]) == "--tebench-target") {
      tebench_target();
      return 0;
    }
    std::ifstream input(argv[1]);
    require(input.good(), "cannot open trace");
    Trace trace;
    try {
      for (std::string line; std::getline(input, line);) {
        std::istringstream stream(line);
        std::vector<std::string> fields;
        for (std::string field; stream >> field;)
          fields.push_back(field);
        if (fields.empty() || fields[0].starts_with('#'))
          continue;
        trace.execute(fields);
      }
      trace.finish();
    } catch (const std::exception &error) {
      stop_trace(error);
    }
    return 0;
  } catch (const std::exception &error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}
