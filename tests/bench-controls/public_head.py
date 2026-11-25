"""Run only on a new ext4 image created by the hosted CPU control."""
import ctypes
import fcntl
import os
from pathlib import Path
import struct
import subprocess
import sys

root=Path(os.environ["OBS_ROOT"])
exe=Path(os.environ["OBS_EXE"])
raw=Path(os.environ["OBS_RAW"])
loop=os.environ["OBS_LOOP"]
scratch=Path(os.environ["RUNNER_TEMP"])
assert root.resolve()==scratch/"read-guard" and loop.startswith("/dev/loop")
backing=subprocess.check_output(["losetup","-n","-O","BACK-FILE",loop],text=True).strip()
assert Path(backing)==scratch/"read-guard.img"
length=64<<20
path=root/"payload"
tile=bytes((i*53+i//256*11+i//32*17+5)%251 for i in range(16384)).replace(b"\xa5",b"\xb7")
page=tile*64
SCALAR=32<<20
def restore(pos=0):
    with path.open("r+b" if path.exists() else "wb",buffering=0) as f:
        f.seek(pos)
        for _ in range((length-pos)//len(page)):f.write(page)
        os.fdatasync(f.fileno())
def cmd(args):subprocess.run(list(map(str,args)),check=True)
def readprofile():
    rows=[l for l in (tr/"uprobe_profile").read_text().splitlines() if ev in l]
    assert len(rows)==1,rows
    return int(rows[0].split()[-1])
def calls(want,args):
    base=readprofile();cmd(args);count=readprofile()-base
    print(f"CHECK stream_prefix observed={count} expected={want}",flush=True)
    assert count==want
def probeoffset():
    names=subprocess.check_output(["nm","-an",exe],text=True).splitlines()
    hits=[l.split() for l in names if "stream_prefix" in l and len(l.split())==3]
    assert len(hits)==1,hits
    virtual=int(hits[0][0],16)
    body=subprocess.check_output(["objdump","-d","--disassemble="+hits[0][2],exe],text=True)
    assert "sfence" in body and ("vmovnt" in body),body
    with exe.open("rb") as f:
        header=f.read(64)
        off=struct.unpack_from("<Q",header,32)[0]
        width,count=struct.unpack_from("<HH",header,54)
        f.seek(off)
        for _ in range(count):
            typ,flags,pos,addr,_,sz,_,_=struct.unpack("<IIQQQQQQ",f.read(width))
            if typ==1 and flags&1 and addr<=virtual<addr+sz: return virtual-addr+pos
    raise RuntimeError("missing executable mapping")
tr=Path("/sys/kernel/tracing");ev="orbite_prefix_guard"
restore()
cmd([raw,path,length,"uring","full"])
with (tr/"uprobe_events").open("a") as f:f.write(f"p:{ev} {exe}:0x{probeoffset():x}\n")
enable=tr/"events/uprobes"/ev/"enable"
try:
    enable.write_text("1")
    calls(1,[exe,"full"])
    for cut in [(SCALAR+x) for x in (1,31,32,33,255,257,511)]+[(2<<20)-1,2<<20,(2<<20)+1]:
        calls(int(cut>=2<<20),[sys.executable,Path(__file__).with_name("cut.py"),cut])
        restore(cut//len(page)*len(page))
    with path.open("rb") as f:
        q=bytearray(32+56)
        struct.pack_into("=QQIIII",q,0,SCALAR,4096,1,0,1,0)
        fcntl.ioctl(f.fileno(),0xc020660b,q,True)
        assert struct.unpack_from("=I",q,20)[0]==1
        logical,physical,n=struct.unpack_from("=QQQ",q,32)
        assert logical<=SCALAR and logical+n>=SCALAR+4096
        bad=(physical+SCALAR-logical)//512
    cmd(["umount",root])
    sectors=int(subprocess.check_output(["blockdev","--getsz",loop]))
    table=f"0 {bad} linear {loop} 0\n{bad} 8 error\n{bad+8} {sectors-bad-8} linear {loop} {bad+8}"
    dm="orbite-read-guard"
    cmd(["dmsetup","create",dm,"--table",table])
    cmd(["mount","-t","ext4","-o","ro,noload","/dev/mapper/"+dm,root])
    assert path.stat().st_size==length
    cmd([raw,path,length,"uring","eio"])
    calls(0,[exe,"eio"])
    print("PASS all confirmed prefixes, tails, both guards and EIO",flush=True)
finally:
    if enable.exists():enable.write_text("0")
    with (tr/"uprobe_events").open("a") as f:f.write(f"-:{ev}\n")
