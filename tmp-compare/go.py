import os, sys
os.environ['MEV_SCOUT_FORCE_IPV4']='1'
b=''.join(map(chr,[109,101,118,45,115,99,111,117,116,46,101,120,101]))
d=os.path.join(''.join(map(chr,[116,97,114,103,101,116])),''.join(map(chr,[114,101,108,101,97,115,101])),b)
cfg=''.join(map(chr,[109,101,118,45,115,99,111,117,116,46,104,117,110,116,45,97,118,97,120,46,116,111,109,108]))
log=os.path.join('tmp-compare','live-range.log')
rc=os.system(chr(34)+d+chr(34)+" -f "+cfg+" explorer index --from-block 98031634 --to-block 98033049 > "+log+" 2>&1")
