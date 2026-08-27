#!/bin/sh
# A stand-in for the compiled tokfold binary, used by the launcher tests.
#
# The launcher's contract is about what it does to a child process -- pass the
# arguments through untouched, hand over the real file descriptors, reproduce
# the exit code, re-raise the signal. None of that needs the real binary, and
# depending on it would mean the launcher could not be tested without a Rust
# toolchain and a completed cross-compile. This script is the child instead:
# it does exactly what it is told to do and nothing else, so a failing test
# points at the launcher rather than at tokfold.
#
# Everything it does is driven by the environment, because the environment is
# the one channel the launcher passes through verbatim without any of the
# behaviour under test depending on it.
#
#   TOKFOLD_FAKE_EXIT    exit with this code (default 0)
#   TOKFOLD_FAKE_SIGNAL  kill self with this signal before doing anything else
#   TOKFOLD_FAKE_MODE    argv (default) | stdin | file
#   TOKFOLD_FAKE_FILE    file to copy to stdout in `file` mode
#   TOKFOLD_FAKE_STDERR  write this line to stderr before exiting

if [ -n "${TOKFOLD_FAKE_SIGNAL:-}" ]; then
	kill -s "$TOKFOLD_FAKE_SIGNAL" $$
fi

case "${TOKFOLD_FAKE_MODE:-argv}" in
argv)
	# One line per argument, delimited, so a test can tell an argument that
	# arrived intact from one a shell would have split, expanded or dropped.
	for arg in "$@"; do
		printf 'arg:[%s]\n' "$arg"
	done
	;;
stdin)
	cat
	;;
file)
	cat "$TOKFOLD_FAKE_FILE"
	;;
esac

if [ -n "${TOKFOLD_FAKE_STDERR:-}" ]; then
	printf '%s\n' "$TOKFOLD_FAKE_STDERR" >&2
fi

exit "${TOKFOLD_FAKE_EXIT:-0}"
