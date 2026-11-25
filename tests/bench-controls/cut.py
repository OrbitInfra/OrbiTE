"""Stop this test consumer at its first read submission, after public admission."""
import ctypes
import errno
import os
import signal
import struct
import sys

libc=ctypes.CDLL(None,use_errno=True)
libc.ptrace.restype=ctypes.c_long
def trace(op,pid,addr=0,data=0):
    rc=libc.ptrace(ctypes.c_uint(op),ctypes.c_uint(pid),ctypes.c_void_p(addr),ctypes.c_void_p(data))
    if rc==-1: raise OSError(ctypes.get_errno(),'ptrace')
    return rc
executable=os.environ.get('OBS_EXE','/observe_eio')
root=os.environ.get('OBS_ROOT','/tmp/ext4')
amount=int(sys.argv[1]) if len(sys.argv)>1 else (32<<20)+512
child=os.fork()
if not child:
    trace(0,0)
    os.kill(os.getpid(),signal.SIGSTOP)
    os.execv(executable,[executable,'cut',str(amount)])
_,s=os.waitpid(child,0)
assert os.WIFSTOPPED(s)
trace(0x4200,child,0,1|8|(1<<20))
trace(24,child)
cut=False
exit_status=None
while True:
    try: pid,status=os.waitpid(-1,0x40000000)
    except ChildProcessError: break
    if os.WIFEXITED(status) or os.WIFSIGNALED(status):
        if pid==child:exit_status=os.waitstatus_to_exitcode(status)
        continue
    sig=os.WSTOPSIG(status)
    if sig==signal.SIGTRAP|0x80:
        info=ctypes.create_string_buffer(128)
        trace(0x420e,pid,ctypes.sizeof(info),ctypes.addressof(info))
        b=info.raw
        if b[0]==1:
            nr,fd,to_submit=struct.unpack_from('=3Q',b,24)
            if nr==426 and to_submit:
                assert not cut
                old=os.stat(root+'/payload').st_size
                assert old==64<<20 and to_submit==1
                os.truncate(root+'/payload',amount)
                print('PROOF external truncate at native single SQE enter',old,amount,flush=True)
                cut=True
    deliver=0 if sig in (signal.SIGSTOP,signal.SIGTRAP,signal.SIGTRAP|0x80) else sig
    try: trace(24,pid,0,deliver)
    except OSError as e:
        if e.errno!=errno.ESRCH: raise
assert cut and exit_status==0,(cut,exit_status)
