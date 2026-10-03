#define _GNU_SOURCE
#include <sys/inotify.h>
#include <sys/vfs.h>
#include <sys/statvfs.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <linux/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#define REQUIRE(x) do { if(!(x)) { perror(#x); return 1; }} while(0)
int main(int argc,char **argv) {
 REQUIRE(argc==2);
 struct statfs artifact;REQUIRE(statfs(argv[1],&artifact)==0 && artifact.f_type==0xef53);
 errno=0;int ro=open(argv[1],O_WRONLY|O_TRUNC);
 REQUIRE(ro<0 && errno==EROFS);
 puts("LOCI_BTRFS_ARTIFACT_READONLY verified=1");
 REQUIRE(geteuid()==1000 && getegid()==1000);
 struct statfs fs; REQUIRE(statfs(".",&fs)==0 && fs.f_type==0x9123683e);
 struct statx sx; REQUIRE(statx(AT_FDCWD,".",AT_SYMLINK_NOFOLLOW,STATX_MNT_ID|STATX_INO,&sx)==0 && (sx.stx_mask&STATX_MNT_ID) && sx.stx_mnt_id>0 && sx.stx_ino>0);
 int fd=inotify_init1(IN_NONBLOCK|IN_CLOEXEC); REQUIRE(fd>=0); int wd=inotify_add_watch(fd,".",IN_CREATE|IN_CLOSE_WRITE|IN_ATTRIB|IN_DELETE); REQUIRE(wd>=0);
 int file=open("observer-created.txt",O_CREAT|O_EXCL|O_WRONLY,0600);REQUIRE(file>=0);REQUIRE(write(file,"btrfs",5)==5);REQUIRE(close(file)==0);
 char buf[4096];ssize_t n=read(fd,buf,sizeof buf);REQUIRE(n>0); unsigned create=0,closed=0;
 for(char *p=buf;p<buf+n;){struct inotify_event *e=(void*)p;create|=e->mask&IN_CREATE;closed|=e->mask&IN_CLOSE_WRITE;p+=sizeof(*e)+e->len;} REQUIRE(create && closed);
 printf("NATIVE_CAPABILITY uid=%u gid=%u statfs=%lx mount_id=%llu inode=%llu inotify_bytes=%zd create=1 close_write=1\n",geteuid(),getegid(),(unsigned long)fs.f_type,(unsigned long long)sx.stx_mnt_id,(unsigned long long)sx.stx_ino,n);
 struct statvfs sv; REQUIRE(statvfs(".",&sv)==0); printf("NATIVE_STATVFS files=%llu ffree=%llu favail=%llu blocks=%llu bfree=%llu bavail=%llu frsize=%llu\n",(unsigned long long)sv.f_files,(unsigned long long)sv.f_ffree,(unsigned long long)sv.f_favail,(unsigned long long)sv.f_blocks,(unsigned long long)sv.f_bfree,(unsigned long long)sv.f_bavail,(unsigned long long)sv.f_frsize);
 REQUIRE(unlink("observer-created.txt")==0);REQUIRE(close(fd)==0);return 0;
}
