## Release note format

Use **openraid vX.Y.Z** as the GitHub release title. The title already appears
above the notes, so do not repeat it or add an H1 in the body.

Start the notes with one short sentence describing the release's value, then use
H2 sections for changes, relevant usage or upgrade notes, downloads, and
verification. Keep change descriptions concise and link the associated issues.
Link detailed documentation instead of repeating the full implementation record.

Save notes as `docs/releases/vX.Y.Z.md`. Release automation uses that file and
attaches the verified platform archives and checksums before publishing.
