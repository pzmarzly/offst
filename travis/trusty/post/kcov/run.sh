#!/usr/bin/env bash

# CODECOV_TOKEN Must be set at this point.

exes=$(find target/${TARGET}/debug -maxdepth 1 -executable -type f)
for exe in ${exes}; do
    echo ">>> kcov: " ${exe} | ts '[%M:%.S]'
    ${HOME}/install/kcov-${TARGET}/bin/kcov \
        --verify \
        --exclude-pattern=/.cargo,/usr/lib \
        --include-pattern="components" \
        target/kcov \
        ${exe} -- --nocapture | ts '[%M:%.S]'
done

# Automatically reads from CODECOV_TOKEN environment variable:
bash <(curl -s https://codecov.io/bash)
