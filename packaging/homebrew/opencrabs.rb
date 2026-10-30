# Cask scaffold for OpenCrabs.
#
# This file is not live: formulae live in Homebrew/homebrew-core and casks in
# Homebrew/homebrew-cask, so neither can be edited from this repository. It is
# kept here as the submission source so the definition is reviewed with the
# change that makes it possible.
#
# Why a cask and not the existing formula: a formula installs `bin/opencrabs`,
# a bare Mach-O, and macOS reads an app icon only from a bundle. A cask can put
# `OpenCrabs.app` in /Applications, which is the only way a Homebrew install
# gets a Dock, Finder and Launchpad icon.
#
# The two sha256 values are filled from the release SHA256SUMS after the first
# release that ships OpenCrabs.app. They are placeholders until then, which is
# why this scaffold must not be submitted as-is.

cask "opencrabs" do
  version "0.5.11"

  on_arm do
    sha256 "<ARM64_SHA256>"
    url "https://github.com/opencrabs/opencrabs/releases/download/v#{version}/opencrabs-v#{version}-macos-arm64.tar.gz",
        verified: "github.com/opencrabs/opencrabs/"
  end

  on_intel do
    sha256 "<AMD64_SHA256>"
    url "https://github.com/opencrabs/opencrabs/releases/download/v#{version}/opencrabs-v#{version}-macos-amd64.tar.gz",
        verified: "github.com/opencrabs/opencrabs/"
  end

  name "OpenCrabs"
  desc "AI agent for the terminal"
  homepage "https://github.com/opencrabs/opencrabs"

  app "OpenCrabs.app"

  # Keep the CLI reachable from a shell without the user touching PATH.
  binary "#{appdir}/OpenCrabs.app/Contents/MacOS/opencrabs"

  zap trash: "~/.opencrabs"
end
