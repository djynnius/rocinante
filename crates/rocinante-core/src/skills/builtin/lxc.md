---
name: lxc
description: "Manage Linux system containers with LXD/Incus: launch, exec, file transfer, snapshots, port forwarding, resource limits. Use when asked to create or manage an LXC/LXD/Incus container, run software in an isolated Linux environment, or debug container networking/limits."
---

# LXC (LXD / Incus)

System containers — full Linux userlands, unlike Docker's process containers. Commands below use `lxc` (LXD); on Incus hosts the client is `incus` with identical subcommands — check with `lxc version || incus version` and substitute.

1. **Inspect before touching anything:**
```bash
lxc list                          # containers, states, IPs
lxc info NAME                     # details of one container
lxc image list images: ubuntu     # available images (remote "images:" and "ubuntu:")
```

2. **Lifecycle:**
```bash
lxc launch ubuntu:24.04 NAME      # create + start
lxc stop NAME                     # graceful; add --force only if it hangs
lxc start NAME
lxc delete NAME                   # only stopped containers; NEVER --force on one you didn't create
```

3. **Run commands inside:**
```bash
lxc exec NAME -- bash -c "apt-get update && apt-get install -y python3"
lxc exec NAME -- systemctl status myservice
```
   Quote the whole inner command after `bash -c` — pipes/globs otherwise run on the host.

4. **Files in and out:**
```bash
lxc file push local.conf NAME/etc/app/app.conf     # host → container (absolute path, no colon)
lxc file pull NAME/var/log/app.log ./app.log       # container → host
lxc file push -r ./dir NAME/opt/                   # recursive
```

5. **Snapshot before anything risky, restore if it goes wrong:**
```bash
lxc snapshot NAME before-upgrade
lxc restore NAME before-upgrade
lxc info NAME                      # lists snapshots at the bottom
lxc delete NAME/before-upgrade     # remove just the snapshot (note the slash)
```

6. **Expose a port** (host 8080 → container 80):
```bash
lxc config device add NAME web proxy listen=tcp:0.0.0.0:8080 connect=tcp:127.0.0.1:80
lxc config device remove NAME web
```

7. **Resource limits:**
```bash
lxc config set NAME limits.memory 2GiB
lxc config set NAME limits.cpu 2
lxc config show NAME               # verify effective config
```

## Rules

- `lxc list` and `lxc info NAME` before any mutating command — know the state first.
- Snapshot (step 5) before upgrades, config surgery, or destructive tests inside the container.
- Never `lxc delete --force` a running container you did not create this session; stop it, confirm with the user, then delete.
- `lxc delete NAME/snap` deletes a SNAPSHOT; `lxc delete NAME` deletes the CONTAINER — check for the slash before running.
- No IPv4 in `lxc list` right after launch: wait a few seconds; still none → `lxc network list` and check the bridge (`lxdbr0`) exists.
- Permission denied talking to the daemon: the user needs to be in the `lxd` group (`sudo usermod -aG lxd $USER`, then re-login) — report this rather than retrying with sudo.
