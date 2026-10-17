# Vendored TypeScript grammar

Source: tree-sitter/tree-sitter-typescript v0.23.2, commit
`f975a621f4e7f532fe322e13c4f79495e0a7b2e7`.

- https://github.com/tree-sitter/tree-sitter-typescript/tree/f975a621f4e7f532fe322e13c4f79495e0a7b2e7
- crates.io archive SHA-256: `6c5f76ed8d947a75cc446d5fccd8b602ebf0cde64ccf2ffa434d873d7a575eff`
- License: MIT, retained in LICENSE (from the same Git commit).
- Only crate build inputs and grammar provenance files are retained.
- TypeScript/TSX `parser.c` files are checked-in generated build inputs. Clean
  builds require no JavaScript packages or generator. Source preprocessing,
  ERROR suppression and runtime registry patching are not used.

Local change: `common/scanner.h` allows automatic semicolon insertion before
`<` in a type context, using the existing `valid_symbols[LOGICAL_OR]` distinction
used for `(` and `[`. This permits semicolonless adjacent generic interface call
signatures while preserving relational expression continuation across newlines.
The Rust language strategy grammar tests cover TS/TSX call signatures, relational
expressions, generic arrows and genuine malformed syntax. Cargo patches this crate
locally so clean offline builds use the same scanner on every platform.

Local grammar change: `export_statement` accepts TypeScript 5.0's
[`export type *` and `export type * as Name`](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-0.html#support-for-export-type-).
`export_specifier` treats a value named `type` as an identifier when exported
directly or renamed, while preserving the distinct `type` modifier in mixed
export clauses. The analyzer retains type-only import/export metadata for type
annotations and excludes erased bindings from runtime call resolution.

Both dialects were regenerated with **tree-sitter-cli 0.24.4** (commit
`fc8c1863e2e5724a0c40bb6e6cfc8631bfe5908b`) and **ABI 14**, preserving the
original ABI and scanner patch. The JavaScript grammar dependency is
tree-sitter-javascript **v0.23.1**, commit
`3a837b6f3658ca3618f2022f8707e29739c91364`, matching the original TypeScript
package's `^0.23.1` dependency. Its unmodified
[grammar.js](https://github.com/tree-sitter/tree-sitter-javascript/blob/3a837b6f3658ca3618f2022f8707e29739c91364/grammar.js)
SHA-256 is `230e330dd914d94297e5e58d53942cd5debf9c069852da93b6a6b1daae1967dc`.
Regenerating the unpatched TypeScript grammar with this exact tool/dependency
reproduces the original `parser.c` and `node-types.json` checksums below.

For regeneration, expose that JavaScript grammar as
`<dependency-directory>/tree-sitter-javascript/grammar.js`, then run in each of
the `typescript` and `tsx` directories:

```sh
NODE_PATH=<dependency-directory> tree-sitter generate --abi 14
```

Only generated `parser.c` changes are required; the node type schema and C
headers remain unchanged. Tests cover both dialects, genuine malformed syntax,
type-only runtime barriers and typed instance calls through type-only barrels.

Upstream file checksums before the local scanner and grammar changes:

```json
{
  "Cargo.toml": "a2912fa21e6bd6c5d7de52b8dfc9ac6891bb97522f0f5ca53cabfbf45d477d54",
  "Cargo.toml.orig": "8a9161a23ef937a8d25cea41cbfedc40db2c37c52a8ee453dfcd57fc31eebf06",
  "README.md": "56d77257cc26b3469abefd371b10d230246ea085431bd26a397d31a1ca7f79c5",
  "common/scanner.h": "da66ef2bd14a3f7ea743e25ba068c6c9aae2c3509db200ff80c4a0e6116e564c",
  "common/define-grammar.js": "c2ac6894e0db6164f56dc339788d9da2da60ce1f81d888e6b74a74fc81db818f",
  "bindings/rust/lib.rs": "f5d90ff81f962148bc0e4ce420f11dc8cc7750a53b65be667b18ca4e23fbc36c",
  "bindings/rust/build.rs": "8e3ee9baddea25add039fc5d12d76fecfe81f1fd175429eb08694ccfcdaf85f4",
  "queries/highlights.scm": "e0c35adb819127bfd4f853fac5419e7d8ba44760246201d04a4a5ce0228a10c5",
  "queries/locals.scm": "c3680f9b56276fb2ccc9f1d5f4d03dbca5d64bdf3e6d52ce5a4ad342cec162d5",
  "queries/tags.scm": "b391288bcc71b513a5df7c9bb232d8bc7418d7e274b125aa0aa4bdd6121a0338",
  "typescript/grammar.js": "7e4109889b2ce2731dde89f82f545a12f1e54a286f72bc874f1aed9965c5d5ae",
  "typescript/src/parser.c": "74fe453edd70f4eae9af0a1050cbd7943d8971d59165b6aaebbaa0a0b716d1aa",
  "typescript/src/scanner.c": "9125013b42cb888379d9be909f1d73dfb75a37626c2cdbf4122718a2b431a6d3",
  "typescript/src/node-types.json": "c790a733fc756b54d4e54dceeb7d2d51e40d8b57136e70277753a75804cce3e3",
  "typescript/src/tree_sitter/parser.h": "a1f6ef161fbaf48a0e10fca90ef5290a062462b307b3898aa562993853b9f80a",
  "typescript/src/tree_sitter/array.h": "4ff743903dc46f5db6aa54f31c6b4d160a8a9779e5b2ab1ee59ae7ebcd850ea1",
  "typescript/src/tree_sitter/alloc.h": "b29c1c9fb7cc82f58c84b376df1297d6e2737a1d655fd356db0859e3c29c2fea",
  "tsx/grammar.js": "19b7bc89bac1dfbb64e01ca15ca52345056028c2582d71762efd675af947a0f6",
  "tsx/src/parser.c": "1902cb53fa7ff5179df89b2eea863165e84c8cc866226419dc26921d8c055885",
  "tsx/src/scanner.c": "d563cd30b2f39718c9ae4292795c5ce03a2ad01954ba3a86ef84c2781a736673",
  "tsx/src/node-types.json": "78b5789145286799a27a0a7ecc36cc1bcb151f94ec7fa631b248459867010c8c",
  "tsx/src/tree_sitter/parser.h": "a1f6ef161fbaf48a0e10fca90ef5290a062462b307b3898aa562993853b9f80a",
  "tsx/src/tree_sitter/array.h": "4ff743903dc46f5db6aa54f31c6b4d160a8a9779e5b2ab1ee59ae7ebcd850ea1",
  "tsx/src/tree_sitter/alloc.h": "b29c1c9fb7cc82f58c84b376df1297d6e2737a1d655fd356db0859e3c29c2fea",
  "LICENSE": "49bf33cf78ef5897e4e161ce1517df7de1ae5042a65b6bcfd44401e0fc606559"
}
```
