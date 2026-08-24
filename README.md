
Comments are worth a note: oxc's codegen treats "minify" as whitespace only,
so comments survive it. The default here drops ordinary and jsdoc comments but
keeps licence text. `oxc-minify` drops licence comments too, so pass
`comments: "none"` for output that matches it exactly.
