#define _GNU_SOURCE
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/uio.h>
#include <sys/syscall.h>
#include <linux/io_uring.h>

static void check(int ok, const char *why) { if (!ok) {perror(why); exit(2);} }
int main(int argc, char **argv) {
    check(argc==5,"usage file length method expected");
    const size_t n = strtoull(argv[2],NULL,10);
    int fd = open(argv[1],O_RDONLY|O_DIRECT);
    check(fd>=0,"open"); uint8_t *buf; check(!posix_memalign((void **)&buf,4096,n+8192),"alloc");
    memset(buf,0xa5,n+8192); struct iovec dest={buf+4096,n};
    int result;
    if (!strcmp(argv[3],"preadv")) {
        ssize_t got=preadv(fd,&dest,1,0); result=got<0?-errno:(int)got;
    } else {
        struct io_uring_params p={0}; int ring=syscall(__NR_io_uring_setup,2,&p); check(ring>=0,"setup");
        size_t sqsz=p.sq_off.array+4*p.sq_entries, cqsz=p.cq_off.cqes+sizeof(struct io_uring_cqe)*p.cq_entries;
        if(p.features&IORING_FEAT_SINGLE_MMAP && sqsz<cqsz) sqsz=cqsz;
        char *sq=mmap(NULL,sqsz,PROT_READ|PROT_WRITE,MAP_SHARED,ring,IORING_OFF_SQ_RING);check(sq!=MAP_FAILED,"sq");
        char *cq=(p.features&IORING_FEAT_SINGLE_MMAP)?sq:mmap(NULL,cqsz,PROT_READ|PROT_WRITE,MAP_SHARED,ring,IORING_OFF_CQ_RING);check(cq!=MAP_FAILED,"cq");
        struct io_uring_sqe *e=mmap(NULL,p.sq_entries*sizeof(*e),PROT_READ|PROT_WRITE,MAP_SHARED,ring,IORING_OFF_SQES);check(e!=MAP_FAILED,"sqe");
        memset(e,0,sizeof(*e)); e->opcode=IORING_OP_READV; e->fd=fd; e->addr=(uintptr_t)&dest; e->len=1;e->user_data=43;
        *(uint32_t *)(sq+p.sq_off.array)=0;__atomic_store_n((uint32_t *)(sq+p.sq_off.tail),1,__ATOMIC_RELEASE);
        int ret=syscall(__NR_io_uring_enter,ring,1,1,IORING_ENTER_GETEVENTS,NULL,0);check(ret==1,"enter");
        check(__atomic_load_n((uint32_t *)(cq+p.cq_off.tail),__ATOMIC_ACQUIRE)==1,"cq count");
        struct io_uring_cqe *receipt=(void *)(cq+p.cq_off.cqes);check(receipt->user_data==43,"cq id");result=receipt->res;
        close(ring);
    }
    size_t changed=0,correct=0,first=n,last=0,zero=0;const size_t confirmed=result>0?(size_t)result:0;
    for(size_t i=0;i<n;i++){uint8_t b=buf[4096+i]; size_t j=i%16384; uint8_t v=(j*53+j/256*11+j/32*17+5)%251;if(v==0xa5)v=0xb7;if(i<confirmed){correct+=b==v;}else if(b!=0xa5){changed++;if(first==n)first=i;last=i;zero+=b==0;}}
    for(size_t i=0;i<4096;i++)check(buf[i]==0xa5&&buf[n+4096+i]==0xa5,"allocation guards");
    printf("PROOF %s %s result=%d confirmed=%zu matching=%zu changed_beyond=%zu zeroed_beyond=%zu first=%zu last=%zu guards=ok\n",argv[1],argv[3],result,confirmed,correct,changed,zero,first,last);
    if(!strcmp(argv[4],"full")) check(result==(int)n&&correct==n&&!changed,"full read");
    if(!strcmp(argv[4],"eof")) check(result==512&&correct==512&&changed>0,"EOF counterexample");
    if(!strcmp(argv[4],"eio")) check(result==-EIO&&changed>0,"EIO counterexample");
    return 0;
}
