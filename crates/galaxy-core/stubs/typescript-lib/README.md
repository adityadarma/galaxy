# TypeScript standard library declarations

The `lib.*.d.ts` files of the [`typescript`](https://www.npmjs.com/package/typescript)
package, version 5.9.3, unchanged. Licensed under Apache-2.0; see `LICENSE.txt`
and `ThirdPartyNoticeText.txt`.

Galaxy reads a project's own `node_modules/typescript/lib` when it exists and
falls back to these files otherwise.

Run `scripts/vendor-typescript-lib.sh` to refresh them and the generated list
in `crates/galaxy-core/src/typescript/lib_files.rs`.
