# Releasing

A release comes from a version tag and from nothing else. No commit, branch or
pull request publishes anything.

## Cut one

1. Set the new version in `Cargo.toml` and commit it.
2. Tag and push:

   ```powershell
   git tag -a v0.5.0 -m "Cadence 0.5.0"
   git push origin v0.5.0
   ```

That is the whole job. The Release workflow does the rest.

Give the tag a suffix, such as `v0.5.0-rc1`, and the release is published as a
pre-release. It stays off the download people land on.

## What the workflow does

It runs on `windows-2025`, whose image already carries the Rust version
`rust-toolchain.toml` asks for, Inno Setup, and the Visual Studio
redistributable the app ships beside itself. The job installs none of them. It
does cache the compiled dependencies between releases, which matters more than
it sounds: gpui, librespot, SDL2 and SQLite all build from source, and on a
cold run that is nearly the whole job.

1. Builds and packages the binary through `scripts/package-windows.ps1`. The
   script stops if the tag and the version in `Cargo.toml` disagree, so a
   mistyped tag never reaches the release page.
2. Uploads the installer and the zip as a workflow artifact.
3. Attests the build with GitHub, so anyone can check a download came from this
   repository:

   ```powershell
   gh attestation verify Cadence-0.5.0-windows-x64-setup.exe --repo ForeverInLaw/windence
   ```

4. Writes the release notes from the commits since the previous tag.
5. Publishes the release with the installer, the zip and their `.sha256` files.

Start the workflow by hand from the Actions tab and it stops after step 2. Use
that to test a packaging change without spending a tag on it.

## The release notes

The Conventional Commit type in front of each subject picks the section. The
wording lives in `cliff.toml`.

| Type | Section |
| --- | --- |
| `feat` | New |
| `fix` | Fixed |
| `perf` | Faster |
| `refactor` | Reworked |
| `docs` | Docs |
| `build`, `chore(deps)` | Build and dependencies |
| `chore`, `ci`, `style`, `test` | Groundwork, in a collapsed block |
| `revert` | left out |

Probes, logging and formatting are real work, and dropping them would be a lie
about what went into the release. They are also not what someone downloading an
installer came to read. So they stay, folded into the Groundwork block, with
repeated subjects collapsed to one line.

To read the notes before tagging:

```powershell
git cliff --strip header --unreleased --tag v0.5.0
```

The workflow downloads `git-cliff` itself. Install it locally only for that
preview:

```powershell
cargo install git-cliff
```
