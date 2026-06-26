# Review: setup-client-postgres.sh r6

## Summary

r6 is still not safe to run as written. The package split the script wants is valid, but line 154 does not actually force the host from PGDG `libpq5` to Fedora `libpq`.

Read-only validation on this host showed:

- `dnf install --assumeno --best --allowerasing libpq` resolves `libpq` through the installed/provider package and plans an upgrade from `libpq5-18.3-1PGDG.f43` to `libpq5-18.4-1PGDG.f43`; it does not install Fedora `libpq`.
- `dnf install --assumeno --best ... postgresql-server-devel ...` still plans `postgresql-private-devel` while `libpq5` is installed. That is the original bad state: the dnf solver does not reject it up front, but the RPM transaction can still hit the `/usr/lib64/libpq.so` file conflict.
- The intended final package split is otherwise coherent. Fedora `libpq.x86_64` owns `/usr/lib64/libpq.so.5` and `/usr/lib64/libpq.so.5.18`; `postgresql-private-devel.x86_64` owns `/usr/lib64/libpq.so`, headers, `pg_config`, and `libpq.pc`; `postgresql-private-libs.x86_64` owns only `/usr/lib64/libpq.so.private18-5*`. That means `postgresql-private-devel` does not conflict with Fedora `libpq` or Fedora `postgresql-private-libs` after the real swap.
- `rpm -q --requires pgadmin4-server` confirms the hard package-name requirement is `libpq5`; `rpm -q --requires gdal-libs` confirms it only needs `libpq.so.5()(64bit)` plus the symbol-version capability, which Fedora `libpq` provides.
- The irreversible cluster purge is still after the package phase, so package failure remains fail-closed with respect to `/var/lib/pgsql/data`.

## BLOCKER

`setup-client-postgres.sh:154` uses a virtual-capability spec that does not perform the advertised swap:

```bash
sudo dnf install -y --best --allowerasing libpq
```

On this host, dnf5 treats `libpq` as already satisfied/provided by PGDG `libpq5` and chooses the newer PGDG `libpq5` package. The resulting state still owns `/usr/lib64/libpq.so`, so the later `postgresql-private-devel` install can still fail with the same RPM file conflict r6 is meant to fix.

Concrete fix: make the transaction target the Fedora package by name/arch or exact NEVRA, and guard that no package-name `libpq5` dependency remains after pgadmin removal. For example:

```bash
if rpm -q libpq5 >/dev/null 2>&1; then
  if rpm -q --whatrequires libpq5 2>/dev/null | grep -v '^no package requires ' | grep -q .; then
    die "libpq5 is still required after removing pgadmin4-server; refusing ambiguous libpq swap"
  fi

  log "replacing PGDG libpq5 with Fedora libpq..."
  sudo dnf swap -y --best --allowerasing libpq5 libpq.x86_64

  rpm -q libpq >/dev/null 2>&1 || die "Fedora libpq is not installed after libpq swap"
  rpm -q libpq5 >/dev/null 2>&1 && die "PGDG libpq5 is still installed after libpq swap"
fi
```

`sudo dnf install -y --best --allowerasing libpq.x86_64` is also an acceptable atomic install/erase transaction after the same dependency guard. The important part is avoiding the bare `libpq` spec, because that is what dnf resolves to PGDG `libpq5`.

## WARN

After the blocker is fixed, the libpq swap should remain a single transaction. Do not replace it with `dnf remove libpq5` followed by `dnf install libpq`, because that creates a transient state where packages needing `libpq.so.5` may be broken if the second command fails.

Concrete fix: keep the replacement atomic with `dnf swap ... libpq.x86_64` or one `dnf install --allowerasing libpq.x86_64` transaction, then verify both `rpm -q libpq` and `rpm -q libpq5` immediately afterward.

## NIT

The section 2 comment currently says `--allowerasing erases the conflicting libpq5 only`, but the current command does not do that. Once the command is fixed, adjust the comment to describe the explicit package-name/arch targeting and the post-transaction guard, not just `--allowerasing`.

Concrete fix: rewrite the comment above the swap to say that the bare `libpq` capability is intentionally avoided because PGDG `libpq5` provides it.

VERDICT: FIXES_REQUIRED
