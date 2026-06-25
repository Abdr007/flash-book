# IDLs for Crucible fuzz harness

Drop the Anchor IDL JSON for `haircut_conservation` here as `haircut_conservation.json`. The simplest
path:

```
# from the program crate root:
anchor build
cp target/idl/haircut_conservation.json ../path/to/fuzz/haircut_conservation/idls/haircut_conservation.json
```

`qedgen probe --fuzz` will look up `target/idl/haircut_conservation.json` and symlink it
into this directory automatically if it exists. Manual copy is the
fallback when discovery doesn't apply (Codama / non-Anchor / hand-rolled).

The IDL must be in Anchor 0.30+ format. Anchor 0.29 IDLs need
`anchor idl convert` first.
