#!/bin/sh
# Build a TestFlight/App Store archive of Styrman.
#
#   ios/scripts/archive.sh            # → ~/Library/Developer/Xcode/Archives (shows in Xcode › Organizer)
#
# Then in Xcode: Window › Organizer › select the archive › Distribute App ›
# TestFlight & App Store › Distribute. Team and bundle id come from Signing.xcconfig.
set -eu
cd "$(dirname "$0")/.."
xcodegen -q
build=$(date +%Y%m%d%H%M)   # App Store Connect needs a new build number every upload
dest="$HOME/Library/Developer/Xcode/Archives/$(date +%Y-%m-%d)"
mkdir -p "$dest"
archive="$dest/Styrman $build.xcarchive"
xcodebuild archive \
  -project Styrman.xcodeproj -scheme Styrman -configuration Release \
  -destination 'generic/platform=iOS' -allowProvisioningUpdates \
  -archivePath "$archive" CURRENT_PROJECT_VERSION="$build" -quiet
echo "Archived build $build: $archive"
echo "Upload it: Xcode › Window › Organizer › Distribute App › TestFlight & App Store."
