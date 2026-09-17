#!/bin/sh
# Cargo runner for this firmware.
#
# This is invoked as:
#     run.sh <console|monitor> <path-to-elf>
#
# Cargo appends the built ELF as the final argument when this is used in `cargo run`. so that's how that works
#
# If `console` is passed in, this will run via probe-rs. If `monitor` is passed in, this will run via defmt-monitor-tui.
# If the respective tool isn't installed, this will prompt you to download it via `cargo install` (will be compiled on your host).

set -eu

# The repo for defmt-monitor. If you don't have git ssh set up this will probably not work. But then how would you have cloned this repo in the first place!
REPO="ssh://git@github.com/bjackson312006/defmt-monitor.git"

# This needs to be defined once in .cargo/config.toml's [env] table. Cargo passes that through to this script.
CHIP="${FIRMWARE_CHIP:?FIRMWARE_CHIP is not set; see .cargo/config.toml}"

# This finds the host platform in case it needs to compile host tooling
HOST="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')" || HOST=""
[ -n "$HOST" ] || {
    echo "run.sh: could not determine the host triple; is rustc on PATH?" >&2
    exit 1
}

mode="${1:-}"
[ -n "$mode" ] || { echo "usage: run.sh <console|monitor> <elf>" >&2; exit 2; }
shift

case "$mode" in
    console) tool="probe-rs" ;;
    monitor) tool="defmt-monitor-tui" ;;
    *) echo "run.sh: unknown mode '$mode' (expected console or monitor)" >&2; exit 2 ;;
esac

# Gets the commit the installed tool was built from.
# This will be empty when it was installed from a local path (development) or is not installed at all, in which case there is nothing
# meaningful to compare against.
installed_rev() {
    cargo install --list 2>/dev/null |
        sed -n 's/^defmt-monitor-tui v[^(]*(.*#\([0-9a-f]\{7,\}\)):$/\1/p'
}

# Gets the remote HEAD for the defmt-monitor-tui repo without cloning.
remote_rev() {
    GIT_TERMINAL_PROMPT=0 \
    GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=5" \
        git ls-remote "$REPO" HEAD 2>/dev/null | cut -f1
}

# Offer to update defmt-monitor-tui when the remote is on a newer version.
#
# Note that this lets the tool run if the read fails, so things like being offline, not having SSH access, or having this
# installed from a path will just skip the check and run the tool as normal. This is not really that bad because this feature of the script
# is just a nice-to-have thing and shouldn't ever block you from flashing or actually using the tool.
#
# Other note: The result of this is cached for a day so this doesn't have to do the check every time you flash
#
# Other other note: This is only relavent for `monitor`, since the `console` path uses probe-rs. So if for some reason this
# auto-updating is being problematic and you just need to flash something rather than debug this, you can just use `console` (normal probe-rs)
offer_update() {
    stamp="${XDG_CACHE_HOME:-$HOME/.cache}/defmt-monitor-tui.update-check"

    # `find -mmin +1440` prints the file only when it is older than a day.
    if [ -f "$stamp" ] && [ -z "$(find "$stamp" -mmin +1440 2>/dev/null)" ]; then
        return 0
    fi

    installed="$(installed_rev)"
    [ -n "$installed" ] || return 0

    remote="$(remote_rev)"
    [ -n "$remote" ] || return 0

    # Only record a successful check, so an offline run retries next time.
    mkdir -p "$(dirname "$stamp")" 2>/dev/null && touch "$stamp" 2>/dev/null || true

    # `installed` is abbreviated, so compare it as a prefix of the full remote hash.
    case "$remote" in
        "$installed"*) return 0 ;;
    esac

    {
        echo
        echo "  A newer defmt-monitor-tui is available."
        echo "      installed: $installed"
        echo "      remote:    $(echo "$remote" | cut -c1-8)"
        echo
    } >&2

    if [ -t 0 ] && [ -r /dev/tty ]; then
        printf '  Update now? [y/N] ' >&2
        read -r reply </dev/tty || reply=""
        case "$reply" in
            [Yy]*)
                echo >&2
                # Not fatal: if the update fails, run the version already installed.
                cargo install --git "$REPO" defmt-monitor-tui --locked --target "$HOST" --force ||
                    echo "  Update failed; continuing with the installed version." >&2
                echo >&2
                ;;
            *) echo "  Skipping." >&2 ;;
        esac
    fi
}

install_hint() {
    case "$1" in
        probe-rs)          echo "cargo install probe-rs-tools --locked --target $HOST" ;;
        defmt-monitor-tui) echo "cargo install --git $REPO defmt-monitor-tui --locked --target $HOST" ;;
    esac
}

install_tool() {
    case "$1" in
        probe-rs)          cargo install probe-rs-tools --locked --target "$HOST" ;;
        defmt-monitor-tui) cargo install --git "$REPO" defmt-monitor-tui --locked --target "$HOST" ;;
    esac
}

if ! command -v "$tool" >/dev/null 2>&1; then
    {
        echo
        echo "  $tool is required for '$mode' mode but was not found on PATH."
        echo
        echo "      $(install_hint "$tool")"
        echo
    } >&2

    # this only prompts when it is a person! for CI it will not hang
    if [ -t 0 ] && [ -r /dev/tty ]; then
        printf '  Install it now? [y/N] ' >&2
        read -r reply </dev/tty || reply=""
        case "$reply" in
            [Yy]*)
                echo >&2
                install_tool "$tool"
                echo >&2
                ;;
            *)
                echo "  Aborted." >&2
                exit 1
                ;;
        esac
    else
        echo "  Not a terminal, so not prompting. Run the command above, then retry." >&2
        exit 1
    fi
fi

case "$mode" in
    console)
        # FIRMWARE_LOG_FORMAT is optional and comes from .cargo/config.toml's [env] table. It is only used for probe-rs run (not the TUI)
        set -- --chip "$CHIP" "$@"
        if [ -n "${FIRMWARE_LOG_FORMAT:-}" ]; then
            set -- --log-format "$FIRMWARE_LOG_FORMAT" "$@"
        fi
        exec probe-rs run "$@"
        ;;
    monitor)
        offer_update
        exec defmt-monitor-tui --chip "$CHIP" "$@"
        ;;
esac
