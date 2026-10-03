# Parse actual LC_ALL=C btrfs filesystem usage -b, not statvfs inode guesses.
/Device size:/ {if($NF !~ /^[0-9]+$/)invalid=1;device=$NF+0}
/Device allocated:/ {if($NF !~ /^[0-9]+$/)invalid=1;allocated=$NF+0}
/Device unallocated:/ {if($NF !~ /^[0-9]+$/)invalid=1;unallocated=$NF+0}
/Device missing:/ {if($NF !~ /^[0-9]+$/)invalid=1;missing=$NF+0}
/Free \(estimated\):/ {minfree=$NF;gsub(/[^0-9]/,"",minfree);minfree+=0}
/^Metadata,DUP:/ {if($2 !~ /^Size:[0-9]+,?$/ || $3 !~ /^Used:[0-9]+$/)invalid=1;metaallocated=$2;gsub(/[^0-9]/,"",metaallocated);metaused=$3;gsub(/[^0-9]/,"",metaused)}
/^Data,single:/ {if($2 !~ /^Size:[0-9]+,?$/ || $3 !~ /^Used:[0-9]+$/)invalid=1;dataused=$3;gsub(/[^0-9]/,"",dataused)}
/^Metadata,/ && !/^Metadata,DUP:/ {unexpected=1}
/^Data,/ && !/^Data,single:/ {unexpected=1}
END {
 if(!device || !allocated || !unallocated || !metaallocated || !minfree || missing || unexpected || invalid){print "LOCI_BTRFS_FAIL usage_missing_or_unknown_profile";exit 1}
 # Additional artifact bytes must leave15% physical device reserve.
 if(reserve+allocated>device*.85 || reserve>minfree){print "LOCI_BTRFS_FAIL usage_physical_reserve";exit 1}
 printf "LOCI_BTRFS_USAGE device=%0.f allocated=%0.f unallocated=%0.f minfree=%0.f metadata_logical_allocated=%0.f metadata_logical_used=%0.f metadata_physical_used=%0.f data_used=%0.f planned_extra=%0.f inode_capacity=unavailable_dynamic\n",device,allocated,unallocated,minfree,metaallocated,metaused,2*metaused,dataused,reserve
 printf "%0.f\n",2*metaused+dataused > physical_out
}
