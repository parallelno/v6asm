I want to improve symbols file generation.
the original design (design\symbols_design.md) stores macro and macroparams symbols of a macro definition. lines, files, and values are from the macro dfefinition has.
here is an actual quote from the design doc.
>- If a macroparam have no default value, value is -1.

I want macro symbols (macro and its parameters) only of the instanced macro.
also I want all symbols with their original names. no need adding postfix to local labels.

================================================================================

1. local labels should not have _N postfix. macro params should not have prefix <macroname>. in the "symbols": {.. }.
2. to resolve embiguety all symbold must be groupped in