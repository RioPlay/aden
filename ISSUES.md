# Aden Issues - Future Work

## Test Results Summary (v0.1.0 - Current)

| Language | init | gen | query | check | Status |
|-----------|------|-----|-------|-------|--------|
| **Rust** | ✅ | ✅ | ✅ | ✅ | Working |
| **Go** | ✅ | ✅ | ✅ | ✅ | Module path = "unknown" (minor) |
| **JavaScript** | ✅ | ✅ | ✅ | ✅ | Duplicate store entries (cosmetic) |
| **Python** | ✅ | ✅ | ✅ | ✅ | Working (uses tree-sitter-language-pack) |

## Confirmed Issues

### 1. JavaScript Duplicate Contracts
**Severity:** Low
**Description:** JavaScript files produce duplicate contract entries during "Stored" phase, but final list deduplicates correctly.
**Root Cause:** TypeScriptExtractor is being called twice or storing twice.
**Example:** `index.js#util` appears twice in "Stored 4 contracts" but only once in list.
**Status:** Low priority - cosmetic issue.

### 2. Go Module Path Not Resolved
**Severity:** Low  
**Description:** Go files show `aden://module/unknown/main.go#main` instead of extracting the actual module path from `go.mod`.
**Root Cause:** Go module path resolution not implemented or failing silently.
**Expected:** Should show something like `aden://module/example.com/project/main.go#main`
**Status:** Low priority - cosmetic issue.

### 3. No Persistent Active Project Setting
**Severity:** Medium
**Description:** Users must specify `--project` flag on every command or change directories manually.
**Status:** Partial fix - `--project` flag implemented. Persistent setting not yet implemented.

### 4. MCP Server Not Responding
**Severity:** High (for MCP users)
**Description:** `aden-mcp` exits with "connection closed: initialize request" when run directly.
**Root Cause:** MCP server requires an initialization request that isn't provided when running without an MCP client.
**Status:** Not fixed - needs proper MCP client to test.

### 5. Source Required for Contracts
**Severity:** Medium
**Description:** Contracts cannot be generated without source files present. No way to define a "virtual" project structure purely from contracts.
**Status:** Not implemented - requires design work.

## Resolved / Not Issues

### Python Parsing - WORKING ✅
Python files ARE being parsed correctly via tree-sitter-language-pack. The earlier report of Python not working was incorrect. Functions, classes, and methods are extracted properly.

### JavaScript Duplicate - Cosmetic Only
The duplicates appear during "Stored" phase but don't affect final anchor list. Low priority.

## Priority Order
1. MCP server connectivity (blocks MCP users)
2. Persistent active project setting
3. Go module path resolution
4. JavaScript duplicate store entries
5. Virtual project structure support