#!/bin/sh
# whereami wrapper: runs the binary inside the signed bundle.
exec "@APP@/Contents/MacOS/whereami" "$@"
