#!/bin/sh
# Links with LLD, much faster than the default bfd linker. A linker wrapper,
# unlike rustflags, leaves compiled crates' fingerprints alone.
exec clang -fuse-ld=lld "$@"
