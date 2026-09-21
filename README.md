# SpareCycles Contributor

> **Work in progress (WIP).** This project is under active development and is not ready for general use. Expect breaking changes, missing features and rough edges. It has not been independently security-reviewed; don't run it on a machine you can't afford to reinstall.

SpareCycles is an **idea in early development, not an existing platform**. There is no running network: no public server, no buyers, no payouts. The idea is a peer-to-peer compute-sharing marketplace where contributors share idle CPU/GPU and buyers pay to run jobs on them, starting with headless Blender rendering.

This is the contributor desktop app, the piece that would run on a contributor's machine.

This repository is the code that runs on **your** machine, published so you can read exactly what it does before you install it.

## What it does

- Detects your hardware and idle state, and lets you set resource limits (CPU/GPU share, etc.).
- Registers your machine with the SpareCycles backend and sends heartbeats.
- Claims render jobs and runs each one inside a sandbox, enforcing your limits at the container level.
- Reports the result back.

## The sandbox

Buyer-submitted jobs are untrusted code. The isolation module is [`src-tauri/src/sandbox.rs`](src-tauri/src/sandbox.rs). It does not implement isolation itself; it drives existing, hardened tooling: **Podman with gVisor (`runsc`)**, inside a Linux runtime the app provisions (a WSL2 distro on Windows).

- gVisor is mandatory. If a machine can't provide it, the app **refuses jobs** rather than running them unsandboxed.
- The sandbox uses **Podman, not Docker**. Jobs run in a separate Podman runtime that SpareCycles installs for you, never on a Docker installation you may already have (Docker Desktop can't run gVisor, and its VM is shared with everything else you run).
- The app targets x86-64 only.

**Status:** the sandbox has not yet had an independent security review. Treat it as pre-release software and don't run it on a machine you can't afford to reinstall until that review is done.

## Building

Requirements: Node.js, [Rust](https://rustup.rs/), and on Windows the MSVC C++ Build Tools.

```
npm install
npm run tauri dev
```

The backend URL defaults to `http://localhost:3000`; override it with `VITE_BACKEND_URL` (see `.env.example`).

Note: the SpareCycles backend, the job runtime build and the buyer-facing tools are not part of this repository, so a self-built app has nothing to connect to unless you supply a compatible backend. Some comments in the source refer to internal docs (`docs/`, `TODO.md`) that are not published here.

## Layout

- `src/` – React + TypeScript UI (dashboard, login, runtime setup, job runner hooks).
- `src-tauri/src/` – Rust side: `system_info.rs` and `gpu.rs` (hardware detection), `provision.rs` (runtime install), `sandbox.rs` and `engine.rs` (job isolation), `job_runner.rs` (claim, run, report).

## Reporting security issues

Please report vulnerabilities privately through GitHub's "Report a vulnerability" (Security tab) rather than a public issue.

## License

[Apache-2.0](LICENSE)
