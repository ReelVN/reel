#!/usr/bin/env python3
import pathlib,resource,subprocess,sys,json
binary=pathlib.Path(sys.argv[1]).resolve()
result=subprocess.run([str(binary),'10000','captures'],text=True,capture_output=True,check=True,timeout=30)
print(result.stdout,end='')
print(json.dumps({'max_rss_KiB':resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss,'platform':'Linux x86_64','scope':'whole source generation + parser + analysis process; includes CST and temporary maps'}),file=sys.stderr)
