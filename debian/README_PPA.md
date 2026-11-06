# Ubuntu PPA Setup Guide

This guide explains how to set up the Debian packaging and Ubuntu PPA upload for all-smi.

## Prerequisites

1. **Launchpad Account**: Create an account at https://launchpad.net
2. **PPA Created**: Create a PPA at https://launchpad.net/~YOUR_USERNAME/+archive/ubuntu/+new
3. **GPG Key**: Generate and upload a GPG key to Launchpad

## Setting up GitHub Secrets

The workflow requires the following secrets to be configured in the GitHub repository under Settings → Secrets and variables → Actions → Repository secrets:

### Required Secrets

1. **GPG_PRIVATE_KEY**
   ```bash
   # Export your GPG private key
   gpg --armor --export-secret-keys YOUR_KEY_ID > private.key
   
   # Copy the contents of private.key to this secret
   ```

2. **GPG_KEY_ID**
   ```bash
   # Find your key ID
   gpg --list-secret-keys --keyid-format=long
   
   # Use the ID after the key type (e.g., "3AA5C34371567BD2")
   # For ed25519 keys: look for the ID after "sec ed25519/"
   # For rsa4096 keys: look for the ID after "sec rsa4096/"
   ```

3. **GPG_PASSPHRASE** (Optional)
   ```bash
   # If your GPG key has a passphrase, add it as a secret
   # This is the passphrase you use to unlock your GPG key
   ```

## Workflow Usage

### Automatic Trigger
The Debian package workflow automatically runs after a successful release build:
1. Create a new release on GitHub
2. The Release workflow builds binaries
3. The Debian package workflow triggers automatically
4. Packages are built and uploaded to PPA

### Manual Trigger
You can also manually trigger the workflow:
1. Go to Actions → "Build and Upload Debian Package"
2. Click "Run workflow"
3. Enter the release tag (e.g., "v0.6.3")
4. Choose whether to upload to PPA

## PPA Configuration

The workflow is configured to upload to: `ppa:lablup/backend-ai`

To change this:
1. Edit `.github/workflows/debian_package.yml`
2. Update the `incoming` field in the dput configuration:
   ```
   incoming = ~YOUR_LAUNCHPAD_USERNAME/ubuntu/YOUR_PPA_NAME/
   ```

## Installing from PPA

Once packages are uploaded and built by Launchpad:

```bash
# Add the PPA
sudo add-apt-repository ppa:lablup/backend-ai
sudo apt update

# Install all-smi
sudo apt install all-smi
```

all-smi does not install, require, or replace an NVIDIA driver. The driver is a `Suggests` relation, which apt does not install by default, so an existing driver from any repository is left untouched and a host with no driver still installs cleanly. Install your driver first, from whichever repository you prefer (the Ubuntu archive or NVIDIA's CUDA repository), then install all-smi.

Do not run `apt install --install-suggests all-smi` on a host whose NVIDIA driver comes from the CUDA repository. `--install-suggests` turns the `Suggests` hint into an installed set, and apt may resolve `nvidia-utils`/`nvidia-driver` to the Ubuntu archive's `*-server` packages, which is the destructive transaction issue #475 describes: the CUDA-repo driver is replaced, its DKMS kernel module is removed, and the install aborts on a firmware file conflict. Plain `apt install all-smi` is the supported path.

## Updating Changelog

To update the changelog from GitHub releases:
```bash
cd debian/
./update-changelog.sh
```

This will fetch all releases from GitHub and format them for the Debian changelog.

## Package Structure

- **Binary Package**: Downloads pre-built binaries from GitHub releases
- **No Compilation**: Uses existing release artifacts to save build time
- **Multi-Architecture**: Supports amd64 and arm64
- **Multi-Distribution**: Builds for Ubuntu 22.04, 24.04, and 24.10

## Rust Toolchain and Cargo.lock Compatibility

### Why rust-1.85-all is Required

The PPA build process uses Ubuntu's **rust-1.85-all** package instead of the default `rustc` package. This is necessary due to Cargo.lock format compatibility:

- **Ubuntu 24.04 (Noble)** default `rustc` package is **Rust 1.75.0**
- **Cargo.lock version 4** requires **Rust 1.78+** to parse
- The repository uses lockfile v4 (generated with newer Rust versions)
- Rust 1.75's cargo cannot parse v4 lockfiles at all

### How It Works

The build process has two phases:

#### 1. Prepare Source Package (before upload)

Run `prepare-source-package.sh` to vendor all Rust dependencies:

```bash
./debian/prepare-source-package.sh
```

This script:
- Runs `cargo vendor debian/vendor` to download all crates
- Creates `.cargo/config.toml` to use vendored sources
- Copies source-based packaging files (`control.source`, `rules.source`)

#### 2. Build on Launchpad (offline)

The `debian/rules` file specifies:

```makefile
# Build-Depends in debian/control
Build-Depends: ..., rust-1.85-all, ...

# Build command uses --frozen for offline builds
cargo-1.85 build --release --frozen
```

Ubuntu's versioned Rust packages provide version-suffixed binaries
(`rustc-1.85`, `cargo-1.85`) to allow multiple Rust versions to coexist.

### Benefits

- **Full Cargo.lock v4 Support**: Rust 1.85 can parse modern lockfile formats
- **Reproducible Builds**: Using `--frozen` ensures exact dependency versions
- **Offline Builds**: Vendored crates work without network access
- **No External Dependencies**: All build tools come from Ubuntu repositories

### Why Not rustup?

Launchpad PPA build environments have **no network access** for security reasons.
This means:
- rustup cannot download toolchains during the build
- cargo cannot download crates from crates.io

Using Ubuntu's official versioned Rust packages + vendored dependencies is the
correct approach for PPA builds.

## Troubleshooting

### GPG Key Issues
- Ensure your GPG key is uploaded to Ubuntu keyserver: `gpg --keyserver keyserver.ubuntu.com --send-keys YOUR_KEY_ID`
- The key must match your Launchpad account email

### PPA Upload Failures
- Check that the version number is unique (can't upload same version twice)
- Verify the GPG signature matches your Launchpad key
- Ensure the distribution name is valid (jammy, noble, oracular)

### Build Failures
- Check Launchpad build logs at https://launchpad.net/~YOUR_USERNAME/+archive/ubuntu/YOUR_PPA/+packages
- Common issues: missing dependencies, architecture mismatches