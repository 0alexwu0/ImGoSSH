#!/bin/sh
# Run programs on an ImGoSSH client using POSIX shell, curl and core utilities.
# No Python, jq, eval, or server-side command interpolation is required.
set -eu

usage() {
    cat <<'HELP'
Usage:
  igworkspace run 'COMMAND' [--cwd /server/mapping] [--timeout SECONDS] [--detach]
  igworkspace logs ID [--follow]
  igworkspace cancel ID
  igworkspace info

Commands run on the client computer. An omitted --cwd selects the first mapping.
HELP
}

fail() { printf '%s\n' "$*" >&2; exit 1; }
number() { case $1 in ''|*[!0-9]*) fail "Expected a numeric value: $1" ;; esac; }

case ${1:-help} in help|-h|--help) usage; exit 0 ;; esac
[ -n "${IMGOSSH_WORKSPACE_URL:-}" ] && [ -n "${IMGOSSH_WORKSPACE_TOKEN:-}" ] ||
    fail 'Run the server setup command copied from ImGoSSH first.'
command -v curl >/dev/null 2>&1 || fail 'curl is required on the SSH server.'

temp=$(mktemp -d "${TMPDIR:-/tmp}/imgossh-run.XXXXXXXX") || exit 1
cleanup() {
    rm -f "$temp/response" "$temp/status" "$temp/stdout" "$temp/stderr" "$temp/id" "$temp/command" "$temp/cwd"
    rmdir "$temp"
}
trap cleanup 0
job_id=
trap 'printf "\nStopped waiting; the client job %s may still be running. Use igworkspace cancel ID to stop it.\n" "$job_id" >&2; exit 130' INT
trap 'exit 143' TERM

api() {
    method=$1 route=$2 destination=$3
    shift 3
    http_status=$(curl --noproxy '*' --connect-timeout 10 --max-time 35 \
        --silent --show-error --request "$method" \
        --header "Authorization: Bearer $IMGOSSH_WORKSPACE_TOKEN" \
        --output "$temp/response" --write-out '%{http_code}' \
        "$@" "${IMGOSSH_WORKSPACE_URL%/}$route") || return 1
    if [ "$http_status" != 200 ]; then
        cat "$temp/response" >&2
        printf '\nWorkspace request failed (HTTP %s).\n' "$http_status" >&2
        return 1
    fi
    cat "$temp/response" > "$destination"
}

fetch_log() {
    log_field=$1 log_offset=$2
    api GET "/shell/jobs/$job_id/$log_field?path=$log_offset" "$temp/$log_field" || return 1
    log_next=$(sed -n '1p' "$temp/$log_field")
    number "$log_next"
    if [ "$(sed -n '2p' "$temp/$log_field")" = 1 ]; then
        printf '\n[Older %s output was truncated; showing retained tail.]\n' "$log_field" >&2
    fi
    if [ "$log_field" = stdout ]; then
        tail -n +3 "$temp/$log_field"
        stdout_offset=$log_next
    else
        tail -n +3 "$temp/$log_field" >&2
        stderr_offset=$log_next
    fi
}

show_job() {
    stdout_offset=0 stderr_offset=0
    while :; do
        # Read status before the logs: a final status guarantees the log
        # readers have finished, so the last fetch includes all final bytes.
        api GET "/shell/jobs/$job_id/status" "$temp/status" || return 1
        job_status=$(sed -n '1p' "$temp/status")
        job_exit=$(sed -n '2p' "$temp/status")
        fetch_log stdout "$stdout_offset" || return 1
        fetch_log stderr "$stderr_offset" || return 1
        if [ "$job_status" != running ] || [ "$follow" = no ]; then
            printf '\n[job %s: %s, exit=%s]\n' "$job_id" "$job_status" "${job_exit:-none}" >&2
            if [ "$follow" = yes ]; then
                if [ "$job_status" = finished ] && [ -n "$job_exit" ]; then
                    return "$((job_exit & 255))"
                fi
                return 1
            fi
            return 0
        fi
        sleep 1
    done
}

action=$1
shift
case $action in
    run)
        run_command= cwd= timeout=300 detach=no have_command=no
        while [ "$#" -gt 0 ]; do
            case $1 in
                --cwd|--timeout)
                    [ "$#" -ge 2 ] || fail "Missing value for $1"
                    if [ "$1" = --cwd ]; then cwd=$2; else timeout=$2; fi
                    shift 2 ;;
                --detach) detach=yes; shift ;;
                -*) fail "Unknown option: $1" ;;
                *)
                    [ "$have_command" = no ] || fail 'Quote the entire client command as one argument.'
                    run_command=$1 have_command=yes
                    shift ;;
            esac
        done
        [ "$have_command" = yes ] || fail 'Missing client command.'
        number "$timeout"
        # Read UTF-8 bytes from files so native curl builds cannot recode
        # non-ASCII commands through their platform's argument encoding.
        printf '%s' "$run_command" > "$temp/command"
        printf '%s' "$cwd" > "$temp/cwd"
        api POST /shell/run "$temp/id" \
            --data-urlencode "command@$temp/command" \
            --data-urlencode "cwd@$temp/cwd" \
            --data-urlencode "timeout_secs=$timeout" || exit 1
        job_id=$(cat "$temp/id")
        number "$job_id"
        printf '[client job %s]\n' "$job_id" >&2
        if [ "$detach" = yes ]; then printf '%s\n' "$job_id"; exit 0; fi
        follow=yes
        show_job ;;
    logs|cancel)
        [ "$#" -gt 0 ] || fail 'Missing job ID.'
        job_id=$1
        number "$job_id"
        shift
        if [ "$action" = cancel ]; then
            [ "$#" -eq 0 ] || fail 'Unexpected cancel arguments.'
            api DELETE "/jobs/$job_id" /dev/null || exit 1
            printf 'Cancellation requested for job %s.\n' "$job_id"
        else
            follow=no
            if [ "${1:-}" = --follow ]; then follow=yes; shift; fi
            [ "$#" -eq 0 ] || fail 'Unexpected logs arguments.'
            show_job
        fi ;;
    info)
        [ "$#" -eq 0 ] || fail 'Unexpected info arguments.'
        api GET /info "$temp/status" || exit 1
        cat "$temp/status"
        printf '\n' ;;
    *) usage >&2; exit 1 ;;
esac
