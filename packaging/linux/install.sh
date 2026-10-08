#!/usr/bin/env bash
set -euo pipefail

source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
install_dir="${HOME}/.local/bin"
session_name="default"
handoff=false
session_set=false

usage() {
    printf '%s\n' \
        'Usage: install.sh [--handoff] [--session NAME] [--bin-dir PATH]' \
        '' \
        'Install the packaged Herdr binary. --session requires --handoff.'
}

while (($#)); do
    case "$1" in
        --handoff)
            handoff=true
            shift
            ;;
        --session|--bin-dir)
            if (($# < 2)) || [[ -z "$2" || "$2" == --* ]]; then
                usage >&2
                exit 2
            fi
            if [[ "$1" == '--session' ]]; then
                session_name="$2"
                session_set=true
            else
                install_dir="$2"
            fi
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$session_set" == true && "$handoff" != true ]]; then
    echo '--session requires --handoff.' >&2
    exit 2
fi

if [[ "$(uname -s)" != 'Linux' || "$(uname -m)" != 'x86_64' ]]; then
    echo 'This package requires Linux x86-64.' >&2
    exit 1
fi

package_binary="$source_dir/herdr"
checksum_file="$source_dir/SHA256SUMS"
if [[ ! -f "$package_binary" || ! -f "$checksum_file" ]]; then
    echo 'This installer must be run from an extracted herdr-focus package.' >&2
    exit 1
fi

if ! (cd -- "$source_dir" && sha256sum --check --status SHA256SUMS); then
    echo 'Package binary checksum verification failed.' >&2
    exit 1
fi

if ! "$package_binary" --version >/dev/null 2>&1; then
    echo 'The packaged Herdr binary failed its --version preflight on this host.' >&2
    exit 1
fi

install -d -- "$install_dir"
if [[ -d "$install_dir/herdr" ]]; then
    echo "Refusing to replace directory: $install_dir/herdr" >&2
    exit 1
fi
install_dir="$(cd -- "$install_dir" && pwd -P)"
destination="$install_dir/herdr"
temporary_binary="$(mktemp "$install_dir/.herdr-install.XXXXXX")"
cleanup() {
    if [[ -n "${temporary_binary:-}" ]]; then
        rm -f -- "$temporary_binary"
    fi
}
trap cleanup EXIT

install -m 755 -- "$package_binary" "$temporary_binary"
mv -fT -- "$temporary_binary" "$destination"
temporary_binary=''
echo "Installed Herdr at $destination."

if [[ "$handoff" == true ]]; then
    if ! "$destination" --session "$session_name" server live-handoff \
        --import-exe "$destination"; then
        echo 'Live handoff failed; the new binary remains installed. No stop command was issued.' >&2
        exit 1
    fi
else
    echo 'Run this installer with --handoff to update the running server.'
fi
