#!/bin/bash
# Deterministic module anchor and documentation generation
# Run this to generate ALL documentation deterministically

set -e

# Step 1: Generate module anchors in MULTIPLE locations for graph discovery
for crate in aden-core aden-cli aden-parse aden-emit aden-asm aden-graph aden-heal aden-propose aden-policy aden-index aden-lsp aden-mcp; do
  lib_file="crates/$crate/src/lib.rs"
  main_file="crates/$crate/src/main.rs"
  
  if [ -f "$lib_file" ]; then
    source_file="$lib_file"
  elif [ -f "$main_file" ]; then
    source_file="$main_file"
  else
    echo "Skipping $crate (no source)"
    continue
  fi
  
  echo "Generating module anchor for $crate..."
  
  # Create in crate src directory
  mkdir -p "contracts/crates/$crate/src"
  cat > "contracts/crates/$crate/src/mod-$crate.adoc" << EOF
:source_file: $source_file
:node-type: module
:last-verified: $(date -u +%Y-%m-%dT%H:%M:%SZ)

[[mod-$crate]]
= $crate

Core module for aden $crate.
EOF
  
  # ALSO create in root contracts/ for graph discovery
  cat > "contracts/mod-$crate.adoc" << EOF
:source_file: $source_file
:node-type: module
:last-verified: $(date -u +%Y-%m-%dT%H:%M:%SZ)

[[mod-$crate]]
= $crate

Core module for aden $crate.
EOF
  
  echo "Created mod-$crate in both locations"
done

# Step 2: Link docs/ to modules
echo ""
echo "Linking documentation to modules..."

# Link architecture.adoc to modules
if ! grep -q "<<mod-aden-" docs/architecture.adoc; then
  echo "" >> docs/architecture.adoc
  echo "== Module Overview" >> docs/architecture.adoc
  echo "" >> docs/architecture.adoc
  echo "See also:" >> docs/architecture.adoc
  for crate in aden-core aden-cli aden-parse aden-emit aden-asm aden-graph aden-heal aden-propose aden-policy aden-index aden-lsp aden-mcp; do
    echo "- <<mod-$crate>>" >> docs/architecture.adoc
  done
  echo "Added module links to architecture.adoc"
fi

# Link ADRs to module documentation
for adr in docs/adr-*.adoc; do
  if ! grep -q "mod-aden" "$adr"; then
    echo "Linking $adr to modules..."
    echo "" >> "$adr"
    echo "== Related Modules" >> "$adr"
    echo "" >> "$adr"
    echo "See: <<mod-aden-core>>, <<mod-aden-cli>>" >> "$adr"
  fi
done

# Link root docs
for doc in README.md AGENTS.md CONTRIBUTING.md NOTICE.md; do
  if [ -f "$doc" ] && ! grep -q "mod-aden" "$doc"; then
    echo "Linking $doc..."
    echo "" >> "$doc"
    echo "== Modules" >> "$doc"
    echo "" >> "$doc"
    echo "See: <<mod-aden-core>>, <<mod-aden-cli>>, <<mod-aden-graph>>" >> "$doc"
  fi
done

echo ""
echo "Done. Run 'aden gen --auto' to regenerate symbol contracts."