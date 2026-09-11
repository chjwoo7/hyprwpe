//! HLSL → GLSL ES normalisation for Wallpaper Engine shaders.
//!
//! The scene shaders are written in HLSL-flavoured GLSL, and the engine's own
//! compiler was lenient in ways GLSL ES 3.0 is not. A host that simply compiles
//! the source as-is therefore fails on shaders that are perfectly correct for
//! Wallpaper Engine — which is what `fxcompile` found: 56 of the library's 91
//! effects were rejected, and every rejection had the same handful of causes.
//!
//! This module is the translation layer, and it is deliberately narrow: it
//! rewrites the constructs the corpus actually contains, each one measured
//! against the driver rather than guessed at.
//!
//! - **Integer literals where a float is wanted.** `3 * amt`, `pointer * 2 - 1`,
//!   `max(0, colour)`. HLSL converts silently; GLSL ES rejects it. This is the
//!   largest class by far.
//! - **`sample` as an identifier.** Reserved in GLSL ES 3.0, and used as a plain
//!   local by several effects.
//! - **`fmod`.** HLSL's name; GLSL has `mod`.
//! - **A macro defined twice.** Two of the engine's headers each define the
//!   `FORMAT_*` constants, and a redefinition is an error even when identical.
//! - **A uniform the JSON marks `"int": true` but the shader declares `float`.**
//!   The material is the contract; declaring it `int` is what makes
//!   `for (int i = u_Min; i < u_Max; i++)` legal.
//!
//! Nothing here is per-shader, and nothing rewrites a value: only types are made
//! explicit. A construct this module does not know about is left exactly as it
//! was written, so the driver still gets to complain — which keeps the remaining
//! unsupported effects visible rather than silently half-working.

use std::collections::BTreeMap;

/// Identifiers that GLSL ES 3.0 reserves but the engine's shaders use freely.
///
/// Only `sample` appears in the corpus; the rest of GLSL's reserved list is left
/// alone so a genuine typo is not papered over.
const RENAMED_IDENTIFIERS: &[(&str, &str)] = &[("sample", "wp_sample")];

/// HLSL functions whose GLSL name differs.
const RENAMED_FUNCTIONS: &[(&str, &str)] = &[("fmod", "mod")];

/// Rewrite a shader body into something GLSL ES accepts.
///
/// Line-oriented, because every construct above is expressed per statement and a
/// whole-file scan would have to re-implement comment and string handling for no
/// benefit. Block comments are tracked across lines so a commented-out `sample`
/// is not renamed.
pub fn normalise(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut defined_macros: BTreeMap<String, String> = BTreeMap::new();
    let mut in_block_comment = false;

    for line in source.lines() {
        if in_block_comment {
            out.push_str(line);
            out.push('\n');
            if line.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }

        // Everything from the first comment marker onwards is preserved
        // verbatim: the trailing `//` carries the material binding we parse
        // later, and a number inside a comment must not be rewritten.
        let cut = line.find("//").into_iter().chain(line.find("/*")).min();
        let (code, tail) = match cut {
            Some(i) => (&line[..i], &line[i..]),
            None => (line, ""),
        };
        if tail.starts_with("/*") && !tail.contains("*/") {
            in_block_comment = true;
        }

        let trimmed = code.trim_start();
        if trimmed.starts_with('#') {
            // Preprocessor. Integer literals in a macro body are left as
            // written: a macro is used in int and float contexts alike, and a
            // guess either way breaks half its uses. An *identical*
            // redefinition, though, is an error - and the engine defines its
            // `FORMAT_*` constants in more than one header.
            if let Some(name) = define_name(trimmed) {
                if defined_macros.contains_key(&name) {
                    // GLSL rejects *any* redefinition, identical or not, and
                    // the engine defines its `FORMAT_*` constants in more than
                    // one header. Guarding the repeat makes the first
                    // definition stand, and is safe inside `#if`/`#else` too:
                    // an inactive branch never defined the name, so its guard
                    // passes.
                    out.push_str("#ifndef ");
                    out.push_str(&name);
                    out.push('\n');
                    out.push_str(code);
                    out.push_str(tail);
                    out.push('\n');
                    out.push_str("#endif\n");
                    continue;
                }
                defined_macros.insert(name, String::new());
            }
            out.push_str(code);
            out.push_str(tail);
            out.push('\n');
            continue;
        }

        // A statement that declares an integer is integer context throughout:
        // in `for (int i = 0; i < 10; i++)` every literal must stay int, and
        // promoting any of them produces a *different* error.
        let int_context = declares_integer(code);
        let code = promote_int_uniform(code, tail);
        out.push_str(&rewrite(&code, !int_context));
        out.push_str(tail);
        out.push('\n');
    }
    out
}

/// The macro name of a `#define`, when the line is one.
fn define_name(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix('#')?.trim_start();
    let rest = rest.strip_prefix("define")?;
    // `#defineX` is not a define.
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    rest.split_whitespace().next().map(String::from)
}

/// Whether a statement declares an `int`/`uint` variable.
fn declares_integer(code: &str) -> bool {
    for (i, _) in code.match_indices("int") {
        // A standalone word: `int i`, not `point` or `print`.
        let before_ok = code[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let after_ok = code[i + 3..].starts_with(|c: char| c.is_whitespace());
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// `uniform float NAME;` -> `uniform int NAME;` when the material says `"int": true`.
///
/// The engine declares some uniforms `float` while the material marks them
/// integer - the JSON is the contract, since it is what the editor exposed - and
/// HLSL let a float be used as a loop index. Correcting the declaration is what
/// makes `for (int i = u_Min; i < u_Max; i++)` legal without casting each use.
fn promote_int_uniform(code: &str, tail: &str) -> String {
    if !code.trim_start().starts_with("uniform ") {
        return code.to_string();
    }
    // The flag lives in the JSON comment, and it must be `true`: `"int":false`
    // is not an integer.
    let marked = tail.split("\"int\"").nth(1).is_some_and(|after| {
        after
            .trim_start_matches(['"', ' ', ':', '\t'])
            .starts_with("true")
    });
    if !marked {
        return code.to_string();
    }
    let Some(space) = code.find(' ') else {
        return code.to_string();
    };
    match code[space + 1..].strip_prefix("float") {
        Some(after) => format!("{}int{after}", &code[..space + 1]),
        None => code.to_string(),
    }
}

/// The character-level rewrite: identifier renames and literal promotion.
fn rewrite(code: &str, rewrite_literals: bool) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    let mut bracket_depth = 0usize;

    while i < bytes.len() {
        let c = bytes[i] as char;

        if c == '[' {
            bracket_depth += 1;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            out.push(c);
            i += 1;
            continue;
        }

        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c.is_ascii_alphanumeric() || c == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            let word = &code[start..i];
            let replacement = RENAMED_IDENTIFIERS
                .iter()
                .chain(RENAMED_FUNCTIONS.iter())
                .find(|(from, _)| *from == word)
                .map(|(_, to)| *to);
            out.push_str(replacement.unwrap_or(word));
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            // A number must be consumed whole before it can be classified:
            // `1e-10` ends in digits that would otherwise look like an integer
            // and be promoted, turning a correct literal into `1e-10.0`.
            let mut is_float = false;
            if code[i..].starts_with("0x") || code[i..].starts_with("0X") {
                i += 2;
                while i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'.' {
                    is_float = true;
                    i += 1;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                }
                // An exponent makes the literal a float whatever else it has.
                if i < bytes.len() && matches!(bytes[i] as char, 'e' | 'E') {
                    let mut j = i + 1;
                    if j < bytes.len() && matches!(bytes[j] as char, '+' | '-') {
                        j += 1;
                    }
                    if j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
                        i = j;
                        while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                            i += 1;
                        }
                        is_float = true;
                    }
                }
                // HLSL suffixes (`1u`, `1.0f`).
                while i < bytes.len() && matches!(bytes[i] as char, 'u' | 'U' | 'f' | 'F') {
                    i += 1;
                }
            }
            let number = &code[start..i];
            let plain_integer =
                !is_float && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
            // A digit continuing an identifier (`vec2`, `g_Texture0`) was taken
            // by the identifier branch; one after a `.` is a fractional part.
            let after_dot = out.ends_with('.');
            if plain_integer && rewrite_literals && bracket_depth == 0 && !after_dot {
                out.push_str(number);
                out.push_str(".0");
            } else {
                out.push_str(number);
            }
            continue;
        }

        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four shapes that dominated the corpus, with the driver's complaint
    /// for each recorded in the module docs.
    #[test]
    fn integer_literals_are_promoted_in_float_expressions() {
        assert_eq!(norm_local("float o4 = 3 * amt;"), "float o4 = 3.0 * amt;");
        assert_eq!(
            norm_local("vec4 p = vec4(pointer * 2 - 1, 0.0, 1.0);"),
            "vec4 p = vec4(pointer * 2.0 - 1.0, 0.0, 1.0);"
        );
        assert_eq!(
            norm_local("gl_FragColor = vec4(max(0, albedo.rgb), albedo.a);"),
            "gl_FragColor = vec4(max(0.0, albedo.rgb), albedo.a);"
        );
        assert_eq!(
            norm_local("vec2 da = mix(a, b, smoothstep(1 - g_Rough, 1, t));"),
            "vec2 da = mix(a, b, smoothstep(1.0 - g_Rough, 1.0, t));"
        );
    }

    /// Integer context must be left alone, or the fix causes a new error.
    #[test]
    fn integer_contexts_are_not_promoted() {
        // A `for` header declares an int, so every literal in it stays int.
        assert_eq!(
            norm_local("for (int i = 0; i < 10; i++) { left += x[i]; }"),
            "for (int i = 0; i < 10; i++) { left += x[i]; }"
        );
        // Array subscripts require an integer.
        assert_eq!(
            norm_local("float v = arr[0] + arr[3];"),
            "float v = arr[0] + arr[3];"
        );
        assert_eq!(norm_local("int count = 4;"), "int count = 4;");
    }

    #[test]
    fn already_unambiguous_numbers_are_untouched() {
        assert_eq!(norm_local("float a = 1.0 + 2e5;"), "float a = 1.0 + 2e5;");
        assert_eq!(norm_local("int mask = 0x1F;"), "int mask = 0x1F;");
        // A digit inside an identifier is not a literal.
        assert_eq!(
            norm_local("vec2 v = g_Texture0Resolution.xy;"),
            "vec2 v = g_Texture0Resolution.xy;"
        );
    }

    #[test]
    fn the_reserved_identifier_is_renamed_everywhere_it_is_used() {
        let src = "vec4 sample = texSample2D(g_Texture0, v_TexCoord);\nreturn sample.rgb;";
        let out = normalise(src);
        assert!(
            !out.contains(" sample"),
            "the declaration is renamed: {out}"
        );
        assert!(out.contains("wp_sample"));
        assert_eq!(
            out.matches("wp_sample").count(),
            2,
            "declaration and use both"
        );
        // A longer identifier containing `sample` must not be touched.
        let other = normalise("float samples = 1.0;");
        assert!(other.contains("samples"), "{other}");
    }

    #[test]
    fn hlsl_function_names_are_mapped() {
        assert_eq!(
            norm_local("v += fmod(g_Time, M_PI / 10) * speed;"),
            "v += mod(g_Time, M_PI / 10.0) * speed;"
        );
    }

    /// A first definition is emitted plainly - no guard is needed for a name
    /// that has not been seen.
    #[test]
    fn the_first_macro_definition_is_not_guarded() {
        let src = "#define FORMAT_DXT1 7\nfloat a = 1.0;\n";
        let out = normalise(src);
        assert_eq!(out.matches("#ifndef").count(), 0, "{out}");
        assert_eq!(out.matches("#define FORMAT_DXT1 7").count(), 1);
    }

    /// The material says the uniform is an integer, so the loop index works.
    #[test]
    fn a_uniform_marked_int_in_the_material_is_declared_int() {
        let src = "uniform float u_Min; // {\"material\":\"Min\",\"int\":true,\"default\":0}\n";
        let out = normalise(src);
        assert!(out.starts_with("uniform int u_Min;"), "{out}");
        // The comment is preserved: it is the binding we parse.
        assert!(out.contains("\"material\":\"Min\""), "{out}");
    }

    #[test]
    fn a_uniform_not_marked_int_keeps_its_type() {
        let src = "uniform float g_Phase; // {\"material\":\"phase\",\"default\":1}\n";
        assert!(normalise(src).contains("uniform float g_Phase;"));
        // `"int":false` is not an integer either.
        let src = "uniform float g_X; // {\"int\":false}\n";
        assert!(normalise(src).contains("uniform float g_X;"));
    }

    /// A number inside a `//` comment is part of the material binding, so it
    /// must not be rewritten.
    #[test]
    fn binding_comments_are_never_rewritten() {
        let src = "uniform float g_Scale; // {\"default\":1,\"range\":[0,16]}\n";
        let out = normalise(src);
        assert!(out.contains("\"default\":1,"), "{out}");
        assert!(out.contains("[0,16]"), "{out}");
    }

    #[test]
    fn a_block_comment_is_left_alone() {
        let src = "/* vec4 sample = 2; */\nreturn 1.0;";
        let out = normalise(src);
        assert!(out.contains("vec4 sample = 2;"), "{out}");
    }

    /// The real failing statement, end to end.
    #[test]
    fn a_real_statement_from_the_library_now_compiles_as_glsl() {
        let src =
            "float phase = (noise.g * M_PI * 2 + v_Params.x * 10 + v_Params.y * 5) * g_Phase;";
        let out = norm_local(src);
        assert_eq!(
            out,
            "float phase = (noise.g * M_PI * 2.0 + v_Params.x * 10.0 + v_Params.y * 5.0) * g_Phase;"
        );
    }

    /// The loop that showed the contract problem: the uniform is `float` in the
    /// shader but `"int": true` in the material.
    #[test]
    fn the_audio_frequency_loop_becomes_legal() {
        let src = "uniform float u_Min; // {\"int\":true}\nfor (int i = u_Min; i < u_Max; i++) {\n  left += g_AudioSpectrum64Left[i];\n}\n";
        let out = normalise(src);
        assert!(out.contains("uniform int u_Min;"), "{out}");
        assert!(out.contains("for (int i = u_Min; i < u_Max; i++)"), "{out}");
        assert!(
            out.contains("g_AudioSpectrum64Left[i]"),
            "the index is untouched"
        );
    }

    /// The regression that mattered: `1e-10` must survive intact, or the
    /// engine's own `rgb2hsv` stops declaring `H` and `S`.
    #[test]
    fn scientific_notation_is_never_split() {
        assert_eq!(
            norm_local("float H = abs((Q.w - Q.y) / (6.0 * C + 1e-10) + Q.z);"),
            "float H = abs((Q.w - Q.y) / (6.0 * C + 1e-10) + Q.z);"
        );
        assert_eq!(norm_local("float a = 2e5 + 1E+3;"), "float a = 2e5 + 1E+3;");
        assert_eq!(norm_local("float b = 1.5e-3 * x;"), "float b = 1.5e-3 * x;");
    }

    /// Two headers define the same constants; GLSL rejects any redefinition, so
    /// the repeat is guarded and the first definition stands.
    #[test]
    fn a_macro_defined_twice_is_guarded() {
        let src = "#define FORMAT_DXT1 7\nfloat a = 1.0;\n#define FORMAT_DXT1 7\n";
        let out = normalise(src);
        assert_eq!(out.matches("#define FORMAT_DXT1").count(), 2);
        assert_eq!(out.matches("#ifndef FORMAT_DXT1").count(), 1, "{out}");
        // The guard must come before the second definition and wrap it.
        let guard = out.find("#ifndef FORMAT_DXT1").unwrap();
        let second = out.rfind("#define FORMAT_DXT1").unwrap();
        assert!(guard < second, "{out}");
        assert!(out[second..].contains("#endif"), "{out}");
    }

    fn norm_local(line: &str) -> String {
        let out = normalise(line);
        out.trim_end_matches('\n').to_string()
    }
}
