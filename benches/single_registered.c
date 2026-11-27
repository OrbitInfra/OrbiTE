#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <liburing.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

static unsigned char pattern(size_t i) { return (unsigned char)((i * 17U + i / 4096U) % 251U); }

int main(int argc, char **argv) {
    struct rlimit limit;
    if (argc != 3 || getrlimit(RLIMIT_MEMLOCK, &limit) != 0) return 2;
    size_t bytes;
    if (strcmp(argv[2],"64")==0) bytes=64U<<20;
    else if (strcmp(argv[2],"4")==0) bytes=4U<<20;
    else return 2;
    fprintf(stdout, "memlock soft=%ju hard=%ju bytes\n", (uintmax_t)limit.rlim_cur, (uintmax_t)limit.rlim_max);
    struct io_uring ring;
    int rc = io_uring_queue_init(2, &ring, 0);
    if (rc < 0) { fprintf(stderr, "queue_init errno=%d (%s)\n", -rc, strerror(-rc)); return 3; }
    unsigned char *buf = NULL;
    if ((rc=posix_memalign((void **)&buf,4096,bytes)) != 0) { fprintf(stderr, "posix_memalign errno=%d\n",rc); return 4; }
    struct iovec iov={.iov_base=buf,.iov_len=bytes};
    rc=io_uring_register_buffers(&ring,&iov,1);
    if (rc<0) { fprintf(stdout,"register_buffers bytes=%zu errno=%d (%s)\n",bytes,-rc,strerror(-rc)); io_uring_queue_exit(&ring); free(buf); return 10; }
    fprintf(stdout,"register_buffers bytes=%zu errno=0\n",bytes);
    int fd=open(argv[1],O_RDWR|O_CREAT|O_EXCL|O_CLOEXEC,0600);
    if (fd<0) return 5;
    for (size_t i=0;i<bytes;++i) buf[i]=pattern(i);
    size_t wrote=0;
    while(wrote<bytes) {
        ssize_t n=write(fd,buf+wrote,bytes-wrote);
        if(n<0 && errno==EINTR) continue;
        if(n<=0) return 6;
        wrote+=(size_t)n;
    }
    if(fdatasync(fd)!=0 || close(fd)!=0) return 7;
    fd=open(argv[1],O_RDONLY|O_DIRECT|O_CLOEXEC);
    if(fd<0) { fprintf(stderr,"open O_DIRECT errno=%d (%s)\n",errno,strerror(errno)); return 8; }
    memset(buf,0xa5,bytes);
    struct io_uring_sqe *sqe=io_uring_get_sqe(&ring);
    if(!sqe) return 11;
    io_uring_prep_read_fixed(sqe,fd,buf,(unsigned)bytes,0,0);
    if((rc=io_uring_submit(&ring))!=1) { fprintf(stderr,"submit result=%d\n",rc); return 12; }
    struct io_uring_cqe *cqe=NULL;
    if((rc=io_uring_wait_cqe(&ring,&cqe))<0) { fprintf(stderr,"wait errno=%d\n",-rc); return 13; }
    int completed=cqe->res;io_uring_cqe_seen(&ring,cqe);
    if(completed!=(int)bytes) { fprintf(stderr,"direct read_fixed cqe=%d\n",completed); return 14; }
    for (size_t i=0;i<bytes;++i) if(buf[i]!=pattern(i)) { fprintf(stderr,"mismatched byte %zu\n",i);return 15; }
    fprintf(stdout,"O_DIRECT READ_FIXED CQE=%d full-bytes=verified\n",completed);
    if((rc=io_uring_unregister_buffers(&ring))<0) { fprintf(stderr,"unregister errno=%d\n",-rc);return 16; }
    close(fd);unlink(argv[1]);io_uring_queue_exit(&ring);free(buf);return 0;
}
