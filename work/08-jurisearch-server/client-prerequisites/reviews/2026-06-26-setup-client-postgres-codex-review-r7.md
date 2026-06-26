# Review: setup-client-postgres.sh r7

## Summary

r7 fixes the r6 blocker. The script now removes the known PGDG package-name dependent first, guards that no installed package still requires the `libpq5` package capability, targets Fedora `libpq` by `name.arch`, and verifies the final rpm state before installing `postgresql-server-devel`.

Read-only validation on this host:

- Current rpm state still matches the problem case before the script runs: `libpq5-18.3-1PGDG.f43.x86_64` is installed, Fedora `libpq` is not installed, and `pgadmin4-server-9.15-1.fc43.x86_64` is installed.
- `rpm -q --whatrequires libpq5` reports only `pgadmin4-server-9.15-1.fc43.x86_64`. `rpm -q --requires pgadmin4-server` includes `libpq5`, so the guard at `setup-client-postgres.sh:156` is checking the right dependency surface after the script's `dnf remove pgadmin4-server`.
- `rpm -q --requires gdal-libs` requires `libpq.so.5()(64bit)` and `libpq.so.5(RHPG_9.6)(64bit)`, not the `libpq5` package name. Fedora `libpq.x86_64` provides those shared-library capabilities, so keeping `gdal-libs` satisfied does not require PGDG `libpq5`.
- `dnf swap --assumeno --best --allowerasing libpq5 libpq.x86_64` resolves to removing `libpq5-18.3-1PGDG.f43.x86_64` and installing `libpq-18.4-3.fc45.x86_64`. Because this was run against the unmodified live rpmdb where `pgadmin4-server` is still installed, the dry-run also pulled PGDG `postgresql17-libs` to satisfy pgadmin's `libpq5 >= 10.0` provide. That does not apply to the script path: `pgadmin4-server` is removed just before the guard, and the guard would abort if any package-name `libpq5` dependency remained.
- `dnf remove --assumeno pgadmin4-server` plans to remove `pgadmin4-server`, dependent `pgadmin4-web`, and unused `python3-mod_wsgi`, so the script's removal step clears the known pgadmin dependency before the swap guard runs.
- `dnf install --assumeno --best --allowerasing libpq.x86_64 postgresql-server postgresql postgresql-server-devel ...` installs Fedora `libpq`, installs `postgresql-server-devel`, and pulls `postgresql-private-devel` in the same clean solver plan with `libpq5` removed. A subsequent `dnf install --assumeno --best "${PKGS[@]}"` after that swap state is therefore expected to install/upgrade the server stack without the original `libpq5` file conflict.
- File ownership confirms the clean split: Fedora `libpq` owns `/usr/lib64/libpq.so.5` and `/usr/lib64/libpq.so.5.18`; `postgresql-private-devel` owns `/usr/bin/pg_config`, `/usr/lib64/libpq.so`, `/usr/lib64/pkgconfig/libpq.pc`, and client headers; `postgresql-private-libs` owns `/usr/lib64/libpq.so.private18-5*`. There is no overlap between Fedora `libpq` and `postgresql-private-devel` on the original conflicting path.
- `bash -n work/08-jurisearch-server/client-prerequisites/setup-client-postgres.sh` is clean. The `set -e` interactions in the swap block are correct: failed rpm probes inside `if` conditions do not trigger the ERR trap, `rpm -q libpq || die` preserves the explicit failure message, and a failed `dnf swap` or package install aborts before the cluster purge.
- The package phase remains before `sudo rm -rf "$PGDATA"`, so the remaining package failure modes stay fail-closed with respect to `/var/lib/pgsql/data`.

## BLOCKER

None.

Concrete fix: no change required.

## WARN

None.

Concrete fix: no change required.

## NIT

None.

Concrete fix: no change required.

VERDICT: GO
