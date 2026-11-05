import os,pty,sys,time,select,struct,fcntl,termios,signal,json
secs=float(sys.argv[1]); out=sys.argv[2]; keys=json.loads(sys.argv[3]); cmd=sys.argv[4:]
pid,fd=pty.fork()
if pid==0: os.execvp(cmd[0],cmd)
fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack('HHHH',40,130,0,0))
t0=time.time(); f=open(out,'wb')
while time.time()-t0<secs:
    r,_,_=select.select([fd],[],[],0.3)
    if r:
        try: d=os.read(fd,65536)
        except OSError: break
        if not d: break
        f.write(d); f.flush()
        if b'\x1b[6n' in d: os.write(fd,b'\x1b[1;1R')
        if b'\x1b[c' in d: os.write(fd,b'\x1b[?62;c')
    while keys and time.time()-t0>=keys[0][0]:
        os.write(fd,keys.pop(0)[1].encode())
os.kill(pid,signal.SIGTERM); time.sleep(1)
try: os.kill(pid,signal.SIGKILL)
except OSError: pass
