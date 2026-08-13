#!/usr/bin/env bash
# Capture a hung LoongArch QEMU from the host when its guest gdbstub is dead.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: ./capture_qemu_hang.sh [--pid PID] [--gcore] [--resume] [--sudo]

Freeze a running qemu-system-loongarch64 process with SIGSTOP, then save host
thread stacks and the guest LoongArch CPU state in a directory below the
current working directory.

Options:
  --pid PID  Target QEMU PID. Required when more than one QEMU is running.
  --gcore    Also write a host core file. This can be as large as guest RAM.
  --resume   Send SIGCONT after collection. The default leaves QEMU stopped.
  --sudo     Run host GDB and gcore through sudo. Required when Linux Yama
             ptrace_scope prevents same-user attachment to QEMU.
  -h, --help Show this help.
EOF
}

pid=''
write_core=false
resume=false
use_sudo=false

while (($#)); do
    case "$1" in
        --pid)
            (($# >= 2)) || { echo '--pid requires a PID' >&2; exit 2; }
            pid="$2"
            shift 2
            ;;
        --gcore)
            write_core=true
            shift
            ;;
        --resume)
            resume=true
            shift
            ;;
        --sudo)
            use_sudo=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "required command not found: $1" >&2
        exit 1
    }
}

require_command gdb
require_command pgrep
require_command ps
require_command kill
if "$use_sudo"; then
    require_command sudo
fi

if [[ -z "$pid" ]]; then
    # Linux /proc/<pid>/comm is limited to 15 bytes, so pgrep -x cannot
    # match qemu-system-loongarch64. Match the complete argv, then verify
    # /proc/<pid>/exe below before sending SIGSTOP.
    mapfile -t qemu_pids < <(pgrep -f '^qemu-system-loongarch64( |$)' || true)
    case ${#qemu_pids[@]} in
        0)
            echo 'no qemu-system-loongarch64 process found' >&2
            exit 1
            ;;
        1)
            pid="${qemu_pids[0]}"
            ;;
        *)
            echo 'multiple qemu-system-loongarch64 processes found; choose one with --pid:' >&2
            ps -o pid=,stat=,pcpu=,args= -p "${qemu_pids[@]}" >&2
            exit 2
            ;;
    esac
fi

[[ "$pid" =~ ^[0-9]+$ ]] || { echo "invalid PID: $pid" >&2; exit 2; }
[[ -d "/proc/$pid" ]] || { echo "process $pid does not exist" >&2; exit 1; }

qemu_bin=$(readlink -f "/proc/$pid/exe")
if [[ ${qemu_bin##*/} != qemu-system-loongarch64 ]]; then
    echo "PID $pid is not qemu-system-loongarch64: $qemu_bin" >&2
    exit 1
fi

timestamp=$(date +%Y%m%d-%H%M%S)
output_dir="$PWD/qemu-hang-$timestamp-pid$pid"
mkdir -p "$output_dir"
output_dir=$(readlink -f "$output_dir")

printf 'pid=%s\nqemu_bin=%s\ncaptured_at=%s\n' \
    "$pid" "$qemu_bin" "$(date -Is)" > "$output_dir/metadata.txt"
ps -o pid,ppid,stat,pcpu,pmem,etime,args -p "$pid" > "$output_dir/process.txt"
ps -L -o pid,tid,stat,pcpu,comm -p "$pid" > "$output_dir/threads-before-stop.txt"

echo "freezing QEMU PID $pid with SIGSTOP"
kill -STOP "$pid"

if [[ $(ps -o stat= -p "$pid" | tr -d ' ') != *T* ]]; then
    echo "PID $pid did not enter a stopped state" >&2
    exit 1
fi

ps -L -o pid,tid,stat,pcpu,comm -p "$pid" > "$output_dir/threads-stopped.txt"

gdb_commands="$output_dir/host-qemu.gdb"
cat > "$gdb_commands" <<'EOF'
set pagination off
set confirm off
set print pretty on
set print elements 64
set logging file __HOST_GDB_LOG__
set logging overwrite on
set logging enabled on
printf "\n=== Host Threads ===\n"
info threads
thread apply all bt 32
printf "\n=== QEMU current_cpu ===\n"
p/x current_cpu
if current_cpu != 0
  set $la = (struct ArchCPU *)current_cpu
else
  printf "current_cpu is NULL in this host thread.\n"
end
printf "\n=== All QEMU LoongArch vCPU states ===\n"
set $cpu = cpus_queue.tqh_first
set $cpu_count = 0
while $cpu != 0 && $cpu_count < 64
  set $la = (struct ArchCPU *)$cpu
  printf "\n--- CPU index %d, CPUState=%p ---\n", $cpu->cpu_index, $cpu
  printf "running=%d stopped=%d stop=%d halted=%u exception_index=%d interrupt_request=0x%x\n", $cpu->running, $cpu->stopped, $cpu->stop, $cpu->halted, $cpu->exception_index, $cpu->interrupt_request
  p/x $la->env.pc
  p/x $la->env.CSR_ERA
  p/x $la->env.CSR_BADV
  p/x $la->env.CSR_ESTAT
  p/x $la->env.CSR_CRMD
  p/x $la->env.CSR_PRMD
  p/x $la->env.CSR_EENTRY
  p/x $la->env.CSR_TLBRENTRY
  p/x $la->env.CSR_TLBRERA
  p/x $la->env.CSR_TLBRBADV
  p/x $la->env.CSR_PGDL
  p/x $la->env.CSR_PGDH
  p/x $la->env.gpr[1]
  p/x $la->env.gpr[3]
  set $cpu = $cpu->node.tqe_next
  set $cpu_count = $cpu_count + 1
end
printf "\ncollected %d CPUState entries\n", $cpu_count
if $cpu_count == 64
  printf "warning: CPU list traversal reached its safety limit\n"
end
set logging enabled off
detach
quit
EOF
sed -i "s|__HOST_GDB_LOG__|$output_dir/host-gdb.txt|" "$gdb_commands"

gdb_prefix=()
if "$use_sudo"; then
    gdb_prefix=(sudo)
elif [[ -r /proc/sys/kernel/yama/ptrace_scope ]] \
    && [[ $(< /proc/sys/kernel/yama/ptrace_scope) != 0 ]]; then
    cat >&2 <<EOF
warning: kernel.yama.ptrace_scope prevents a same-user GDB attach in this environment.
rerun with --sudo to collect host thread stacks and guest registers.
EOF
fi

echo 'collecting host thread stacks and guest CPU state'
if ! "${gdb_prefix[@]}" gdb -q -batch "$qemu_bin" -p "$pid" -x "$gdb_commands" \
    > "$output_dir/gdb-stdout.txt" 2> "$output_dir/gdb-stderr.txt"; then
    echo 'host GDB collection failed; inspect gdb-stderr.txt' >&2
fi

if "$write_core"; then
    require_command gcore
    echo "writing host core to $output_dir/qemu-core.$pid (may be very large)"
    "${gdb_prefix[@]}" gcore -o "$output_dir/qemu-core" "$pid" > "$output_dir/gcore-stdout.txt" \
        2> "$output_dir/gcore-stderr.txt" || {
        echo 'gcore failed; inspect gcore-stderr.txt' >&2
    }
fi

if "$resume"; then
    echo "resuming QEMU PID $pid with SIGCONT"
    kill -CONT "$pid"
else
    echo "QEMU PID $pid remains stopped; resume it with: kill -CONT $pid"
fi

echo "capture saved to: $output_dir"
echo "start with: $output_dir/host-gdb.txt"
