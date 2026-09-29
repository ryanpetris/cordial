#!/usr/bin/env python3
"""Check development radio discovery and multiplexing. Never pair or inject HID."""
import argparse
import json
import time
from check_usb import Console


def send(console, command, args=None):
    console.id += 1
    console.write(json.dumps(dict(v=1, id=console.id, cmd=command, args=args or {})).encode() + b"\n")
    return console.id


def wait_ready(console, initial):
    console.ok("session.heartbeat")
    request = send(console, "adapter.wait_ready")
    pending = {request}
    deadline = time.monotonic() + 35
    waiting, status = False, None
    while pending:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Adapter readiness timed out")
        try:
            message = console.read(min(5, remaining))
        except TimeoutError:
            if time.monotonic() >= deadline:
                raise
            pending.add(send(console, "session.heartbeat"))
            continue
        assert message["type"] == "response" and message["id"] in pending and message["ok"], message
        if message["id"] == request:
            if message["done"]:
                assert message["result"]["state"] == "ready", message
                status = message["result"]["status"]
                assert all(status[k] == initial[k] for k in ("adapter_id", "boot_id", "session_id")), status
                assert status["radio_ready"] and status["storage_ready"], status
            else:
                assert not waiting and message["result"] == {"state": "initializing"}, message
                waiting = True
        else:
            assert message["done"], message
        if message["done"]:
            pending.remove(message["id"])
    return status


def collect(console, pending, timeout=8):
    deadline = time.monotonic() + timeout
    results, candidates, events = {}, {}, []
    while pending:
        message = console.read(max(0.01, deadline - time.monotonic()))
        if message["type"] == "event":
            assert "id" not in message
            events.append(message)
            if message["event"] == "discovery.result":
                candidate = message["data"]
                candidates[candidate["candidate_id"]] = candidate
            else:
                assert message["event"] != "protocol.error", message
        else:
            request = message["id"]
            assert request in pending, message
            results.setdefault(request, []).append(message)
            if message["done"]:
                pending.remove(request)
    return results, candidates, events


def main():
    if not __debug__:
        raise SystemExit("Run without -O: checks require assertions enabled.")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--adapter-id", required=True)
    args = parser.parse_args()
    console = Console(args.port)
    try:
        initial = console.ok("adapter.status")
        assert initial["adapter_id"] == args.adapter_id
        assert initial["build_profile"] == "development"
        initial = wait_ready(console, initial)
        capabilities = console.ok("adapter.capabilities")
        assert {"classic", "ble"} <= set(capabilities)
        console.ok("session.monitor.set", {"enabled": True})
        scan = send(console, "discovery.scan", {"transport": "both", "duration_ms": 3000})
        heartbeat = send(console, "session.heartbeat")
        snapshot = send(console, "device.list")
        results, candidates, _ = collect(console, {scan, heartbeat, snapshot})
        assert all(rows[-1]["ok"] for rows in results.values()), results
        assert results[scan][-1]["result"]["count"] == len(candidates)
        assert results[snapshot][-1]["result"]["count"] == len(results[snapshot]) - 1
        print(json.dumps({"radio_ready": True, "candidate_count": len(candidates),
            "transports_seen": sorted({c["transport"] for c in candidates.values()}),
            "multiplexed_heartbeat_and_snapshot": True}), flush=True)
        scan = send(console, "discovery.scan", {"duration_ms": 0})
        cancellation = send(console, "request.cancel", {"request_id": scan})
        results, _, _ = collect(console, {scan, cancellation})
        assert results[cancellation][-1]["ok"]
        assert results[scan][-1]["error"]["code"] == "cancelled"
        scan = send(console, "discovery.scan", {"duration_ms": 0})
        console.new_session()  # DTR loss must stop discovery and discard its IDs.
        final = console.ok("adapter.status")
        assert final["boot_id"] == initial["boot_id"] and not final["pending"] and not final["monitor"]
        assert final["counts"]["paired"] == initial["counts"]["paired"]
        print("Radio discovery, cancellation and session teardown passed; no pairing performed.")
    finally:
        console.close()


if __name__ == "__main__":
    main()
