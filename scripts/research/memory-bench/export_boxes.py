"""Export agentd stores + harness session files from a user's box to a private local dir."""
import os
import sys
import tarfile
from pathlib import Path

from daytona import Daytona, DaytonaConfig

OUT = Path.home() / "membench" / "private" / "boxes"
cfg = DaytonaConfig(api_key=os.environ["DAYTONA_API_KEY"])
d = Daytona(cfg)
for arg in sys.argv[1:]:
    user, sid = arg.split("=")
    sb = d.get(sid)
    print("==", user, sid, sb.state)
    cmd = (
        "set -e; rm -f /tmp/arbos-export.tgz; cd /work && sudo tar czf /tmp/arbos-export.tgz "
        "$(sudo find . -maxdepth 3 -path '*/.arbos/*' \\( -name agentd -o -name pi -o -name prime -o -name audit.jsonl \\) -printf '%p ') "
        "&& sudo chown daytona /tmp/arbos-export.tgz && ls -la /tmp/arbos-export.tgz"
    )
    r = sb.process.exec(cmd, timeout=120)
    print(r.exit_code, r.result)
    dest = OUT / user
    dest.mkdir(parents=True, exist_ok=True)
    data = sb.fs.download_file("/tmp/arbos-export.tgz")
    tgz = dest / "export.tgz"
    tgz.write_bytes(data)
    with tarfile.open(tgz) as t:
        t.extractall(dest, filter="data")
    sb.process.exec("rm -f /tmp/arbos-export.tgz", timeout=30)
    print("extracted to", dest)
