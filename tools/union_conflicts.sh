#!/bin/bash
# Resolve git conflict blocks in the given files by taking the union of both sides
# (ours first, then lines from theirs not already in ours). Meant for append-only
# registration files. Afterwards, `pub mod`/`mod` declaration lines inside a resolved
# block are hoisted above the first non-declaration line of that block, so Rust
# module lists and function bodies don't get interleaved.
for f in "$@"; do
  awk '
    /^<<<<<<< /{inblk=1; side=1; n1=0; n2=0; next}
    /^\|\|\|\|\|\|\| /{ if(inblk){side=3; next} }
    /^=======$/{ if(inblk){side=2; next} }
    /^>>>>>>> /{ if(inblk){
        m=0; k=0
        for(i=1;i<=n1;i++){ seen[a[i]]=1; if(a[i] ~ /^(pub )?mod [a-z_0-9]+;/) mods[++m]=a[i]; else rest[++k]=a[i] }
        for(i=1;i<=n2;i++){ if((b[i] in seen) && b[i] !~ /^[ \t]*$/) continue; seen[b[i]]=1;
            if(b[i] ~ /^(pub )?mod [a-z_0-9]+;/) mods[++m]=b[i]; else rest[++k]=b[i] }
        for(i=1;i<=m;i++) print mods[i]
        for(i=1;i<=k;i++) print rest[i]
        delete seen; delete mods; delete rest; inblk=0; next } }
    { if(!inblk){print; next}
      if(side==1){a[++n1]=$0} else if(side==2){b[++n2]=$0} }
  ' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
done
