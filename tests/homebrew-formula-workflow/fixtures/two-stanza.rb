class AllSmi < Formula
  desc      "GPU ‘top’ for NVIDIA/Jetson/Apple Silicon/Tenstorrent"
  homepage  "https://github.com/lablup/all-smi"
  version "0.25.0"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/lablup/all-smi/releases/download/v0.25.0/all-smi-macos-aarch64.zip"
      sha256 "d090d6abbda5ac0c2fc06c0924b70845b32fd02ce6d1fca8f3ddf6fce1cad814"
    end
    if Hardware::CPU.intel?
      url "https://github.com/lablup/all-smi/releases/download/v0.25.0/all-smi-macos-x86_64.zip"
      sha256 "1111111111111111111111111111111111111111111111111111111111111111"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/lablup/all-smi/releases/download/v0.25.0/all-smi-linux-aarch64.tar.gz"
      sha256 "3bd2a8859df3edfc17c08b365f510d5531bfee5c3a182199ba697d44992c7df9"
    end
    if Hardware::CPU.intel?
      url "https://github.com/lablup/all-smi/releases/download/v0.25.0/all-smi-linux-x86_64.tar.gz"
      sha256 "59c3742ab4de9f6de3c5b3830a3c517ae59e14bc1a6c64f521f4687d1f41a051"
    end
  end

  def install
    bin.install "all-smi"
    man1.install "all-smi.1"
  end

  test do
    output = shell_output("#{bin}/all-smi --version")
    assert_match(/all-smi\s+#{version}/, output)
  end
end
