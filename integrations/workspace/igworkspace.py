#!/usr/bin/env python3
"""Access an ImGoSSH client's shared workspace from its connected SSH server."""
import argparse
import json
import os
from pathlib import Path
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, build_opener, ProxyHandler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=os.environ.get("IMGOSSH_WORKSPACE_URL"))
    # Credentials come from the environment, avoiding tokens in process arguments.
    sub = parser.add_subparsers(dest="action", required=True)
    sub.add_parser("info")
    ls = sub.add_parser("ls")
    ls.add_argument("path", nargs="?", default="")
    cat = sub.add_parser("cat")
    cat.add_argument("path")
    get = sub.add_parser("get")
    get.add_argument("path", help="path relative to the client's shared directory")
    get.add_argument("destination", type=Path)
    put = sub.add_parser("put")
    put.add_argument("source", type=Path)
    put.add_argument("path", help="path relative to the client's shared directory")
    mkdir = sub.add_parser("mkdir")
    mkdir.add_argument("path")
    run = sub.add_parser("run")
    run.add_argument("command", help="PowerShell on Windows, /bin/sh on Unix")
    run.add_argument("--cwd", default="")
    run.add_argument("--timeout", type=int, default=300)
    run.add_argument("--detach", action="store_true")
    logs = sub.add_parser("logs")
    logs.add_argument("id", type=int)
    logs.add_argument("--follow", action="store_true")
    cancel = sub.add_parser("cancel")
    cancel.add_argument("id", type=int)
    args = parser.parse_args()
    token = os.environ.get("IMGOSSH_WORKSPACE_TOKEN")
    if not args.url or not token:
        parser.error("set IMGOSSH_WORKSPACE_URL and IMGOSSH_WORKSPACE_TOKEN using the client setup command")
    # Always bypass HTTP proxies: this endpoint is on the SSH server's loopback.
    opener = build_opener(ProxyHandler({}))

    def api(method, route, data=None, path=None, binary=False):
        if path is not None:
            route += "?" + urlencode({"path": path})
        if isinstance(data, dict):
            data = json.dumps(data).encode("utf-8")
        request = Request(args.url.rstrip("/") + route, data=data, method=method,
                          headers={"Authorization": "Bearer " + token})
        with opener.open(request, timeout=35) as response:
            raw = response.read()
        return raw if binary else json.loads(raw)

    def wait_job(job_id, follow):
        previous = {"stdout": "", "stderr": ""}
        while True:
            job = api("GET", "/jobs/" + str(job_id))
            for key, stream in (("stdout", sys.stdout), ("stderr", sys.stderr)):
                text = job[key]
                # Snapshots retain the last MiB per stream. Display a new tail
                # when the old prefix has rolled out of the retained buffer.
                if text.startswith(previous[key]):
                    delta = text[len(previous[key]):]
                elif text == previous[key]:
                    delta = ""
                else:
                    delta = "\n[log buffer rolled; showing retained tail]\n" + text
                stream.write(delta)
                stream.flush()
                previous[key] = text
            if job["status"] != "running" or not follow:
                print("\n[job {}: {}, exit={}]".format(job_id, job["status"], job["exit_code"]), file=sys.stderr)
                if job["stdout_truncated"] or job["stderr_truncated"]:
                    print("[only the last 1 MiB per output stream is retained]", file=sys.stderr)
                if follow:
                    return job["exit_code"] if job["status"] == "finished" and job["exit_code"] is not None else 1
                return 0
            time.sleep(0.3)

    if args.action == "info":
        print(json.dumps(api("GET", "/info"), ensure_ascii=False, indent=2))
    elif args.action == "ls":
        entries = api("GET", "/list", path=args.path)["entries"]
        for entry in sorted(entries, key=lambda e: (not e["directory"], e["name"])):
            print("{:>12}  {}{}".format(entry["size"], entry["name"], "/" if entry["directory"] else ""))
    elif args.action in ("cat", "get"):
        data = api("GET", "/file", path=args.path, binary=True)
        if args.action == "cat":
            sys.stdout.buffer.write(data)
        else:
            args.destination.write_bytes(data)
    elif args.action == "put":
        api("PUT", "/file", data=args.source.read_bytes(), path=args.path)
    elif args.action == "mkdir":
        api("POST", "/mkdir", path=args.path, data=b"")
    elif args.action == "run":
        job_id = api("POST", "/run", data={"command": args.command, "cwd": args.cwd, "timeout_secs": args.timeout})["id"]
        print("[job {}]".format(job_id), file=sys.stderr)
        if args.detach:
            print(job_id)
        else:
            return wait_job(job_id, True)
    elif args.action == "logs":
        return wait_job(args.id, args.follow)
    elif args.action == "cancel":
        api("DELETE", "/jobs/" + str(args.id))
        print("Cancellation requested")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HTTPError as error:
        print(error.read().decode("utf-8", errors="replace"), file=sys.stderr)
        sys.exit(1)
    except (URLError, OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        print("\nJob continues on the client; use 'cancel ID' to stop it.", file=sys.stderr)
        sys.exit(130)
