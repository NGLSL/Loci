#define _GNU_SOURCE
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
#define OK(x) do {if(!(x)){perror(#x);return 1;}} while(0)
/* Fixture creation only: no Engine/acceptance behavior is simulated here. */
static const char *names[]={"report","source","notes","backup","invoice","image","video","日志","报告","记录","rr_source","ii_notes","ia_backup"};
static const char *exts[]={"txt","rs","md","log","docx","png","json"};
int main(int argc,char **argv){
 OK(argc==5 && geteuid()==1000 && getegid()==1000);
 char *end;unsigned long count=strtoul(argv[2],&end,10);OK(*end==0 && count>=4 && count<=20000 && count%4==0);
 unsigned long reserve=strtoul(argv[3],&end,10);OK(*end==0 && reserve>=64*1024*1024UL);
 const char *root=argv[1],*token=argv[4];OK(strlen(root)<512 && root[0]=='/' && strlen(token)<100 && strchr(token,'/')==NULL);
 int created_root=mkdir(root,0700)==0;if(!created_root)OK(errno==EEXIST);
 struct stat st;OK(lstat(root,&st)==0 && S_ISDIR(st.st_mode) && st.st_uid==1000);
 int rootfd=open(root,O_RDONLY|O_DIRECTORY|O_NOFOLLOW);OK(rootfd>=0);
 int own=created_root?openat(rootfd,".loci-owned",O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,0600):-1;if(!created_root)errno=EEXIST;
 if(own>=0){OK(write(own,token,strlen(token))==(ssize_t)strlen(token));OK(fsync(own)==0);OK(close(own)==0);}
 else {OK(errno==EEXIST);char b[101]={0};own=openat(rootfd,".loci-owned",O_RDONLY|O_NOFOLLOW);OK(own>=0);ssize_t n=read(own,b,100);OK(n==(ssize_t)strlen(token) && memcmp(b,token,n)==0);OK(close(own)==0);}
 /* Owner marker lives in parent; the searchable data root has exactly N*50 entries. */
 if(mkdirat(rootfd,"data",0700)<0)OK(errno==EEXIST);
 int datafd=openat(rootfd,"data",O_RDONLY|O_DIRECTORY|O_NOFOLLOW);OK(datafd>=0);OK(fstat(datafd,&st)==0 && st.st_uid==1000);OK(close(datafd)==0);
 char (*dirs)[512]=calloc(count,sizeof(*dirs));OK(dirs!=NULL);
 for(unsigned long i=0;i<count;i++){
  if(i%100==0){struct statvfs v;OK(fstatvfs(rootfd,&v)==0);uint64_t available=(uint64_t)v.f_bavail*v.f_frsize,total=(uint64_t)v.f_blocks*v.f_frsize;OK(available>reserve && available-reserve>=total/100*15);if(v.f_files)OK(v.f_favail>=2048);}
  const char *parent=i%4==0?root:i%4==3?dirs[i-3]:dirs[i-1];
  int n=snprintf(dirs[i],512,i%4==0?"%s/data/dir%05lu":"%s/dir%05lu",parent,i);OK(n>0 && n<512);
  if(mkdir(dirs[i],0700)<0)OK(errno==EEXIST);
  int d=open(dirs[i],O_RDONLY|O_DIRECTORY|O_NOFOLLOW);OK(d>=0);OK(fstat(d,&st)==0 && st.st_uid==1000);
  for(unsigned j=0;j<49;j++){
   unsigned long id=i*49+j;char name[128];n=snprintf(name,sizeof name,"%s_%08lu.%s",names[id%13],id,exts[id%7]);OK(n>0 && n<(int)sizeof name);
   int f=openat(d,name,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,0600);
   if(f>=0)OK(close(f)==0);else {OK(errno==EEXIST);OK(fstatat(d,name,&st,AT_SYMLINK_NOFOLLOW)==0 && S_ISREG(st.st_mode) && st.st_uid==1000 && st.st_size==0);}
  }
  OK(close(d)==0);if((i+1)%1000==0){printf("LOCI_BTRFS_FIXTURE_PROGRESS directories=%lu requested=%lu\n",i+1,count);fflush(stdout);}
 }
 OK(close(rootfd)==0);free(dirs);printf("LOCI_BTRFS_FIXTURE_CREATED entries=%lu directories=%lu real_regular_files=%lu run_id=%s\n",count*50,count,count*49,token);return 0;
}
