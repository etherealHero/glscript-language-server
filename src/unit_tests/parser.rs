use glscript_language_server::parser::{Rule, parse};

fn emit_eq(content: &str) -> bool {
    content
        == parse(content)
            .into_iter()
            .fold("".to_owned(), |acc, n| acc + n.text.as_str())
}

// TODO: more tests

#[test]
fn emit_equal_content() {
    assert!(emit_eq(indoc::indoc! {r#"
        /**
         * @module
         */

        import "common.js";

        function exec() {
            #include <string.js>;

            var x = `lorem
                ipsum\`
                dolor`;

            return x;
        }

        var result =
        #text
        exec double = 'exec'
        #endtext

        result.replace('\'exec\'', exec()); // 'exec double = lorem\r\n        ipsum\\`\r\n        dolor'
    "#}));
}

#[test]
fn parse_import_statements() {
    fn test(c: &str) -> bool {
        emit_eq(c) && parse(c).into_iter().any(|n| n.rule == Rule::IncludePath)
    }

    assert!(test("import 'util.js'"), "single quote");
    assert!(test("import \"util.js\""), "with double qoutes");
    assert!(test("import \"dir/util.js\""), "slash");
    assert!(test("import \"dir\\util.js\""), "not escaped backslash");
    assert!(test("import \"dir\\\\util.js\""), "escaped backslash");
    assert!(test("import \"../util.js\""), "relative path");
    assert!(test("#include <util.js>"), "include keyword");
}

#[test]
fn skip_import_inside_nested_statements() {
    fn test(c: &str) -> bool {
        emit_eq(c) && !parse(c).into_iter().any(|n| n.rule == Rule::IncludePath)
    }

    assert!(test("// import 'util.js'"), "commnet");
    assert!(test("/* import 'util.js' */"), "multiline commnet");
    assert!(test("/*\nimport 'util.js'\n*/"), "multiline commnet \\n");
    assert!(test("#text\nimport 'util.js'\n#endtext\n"), "text region");
    assert!(test("#sql\nimport 'util.js'\n#endsql\n"), "sql region");
    assert!(test("\"import 'util.js'\""), "plain literal");
    assert!(test("`\nimport 'util.js'\n`"), "backtick \\n");
    assert!(test("x = \"import 'util.js'\""), "with untracked node");
    assert!(test("x = `\nimport 'util.js'\n`;"), "untracked node \\n");
}

#[test]
#[ignore = "TODO: rewrite"]
fn escape_brackets_in_literals() {
    fn test(c: &str) -> bool {
        let mut start_cnt = 0;
        let mut end_cnt = 0;

        let mut start = "".to_string();
        let mut emit_content = "".to_string();
        let mut end = "".to_string();

        parse(c).into_iter().for_each(|n| match n.rule {
            Rule::MultiLineCommentOpenBracket | Rule::TemplateStringBracket if start_cnt == 0 => {
                start.push_str(n.text.as_str());
                start_cnt += 1;
            }
            Rule::TemplateStringChars | Rule::LineTerminator => {
                emit_content.push_str(n.text.as_str())
            }
            Rule::MultiLineCommentCloseBracket | Rule::TemplateStringBracket if start_cnt > 0 => {
                end.push_str(n.text.as_str());
                end_cnt += 1;
            }
            kind => panic!("unexpected node {:?}", kind),
        });

        emit_eq(c)
            && start_cnt == 1
            && end_cnt == 1
            && start.to_owned() + emit_content.as_str() + end.as_str() == c
    }

    assert!(test("'lorem\\'ipsum'"), "single quote");
    assert!(test("\"lorem\\\"ipsum\""), "double quote");
    assert!(test("`lorem\\`ipsum`"), "backtick");
    assert!(test("`lorem\\`\nipsum`"), "backtick \\n");
    assert!(test("/* lorem \\*/ipsum */"), "comment");
    assert!(test("/* lorem \\*/\nipsum */"), "comment \\n");
}

#[test]
fn skip_literal_inside_single_line_comment() {
    let mut nodes = parse("// 'not a lit'").into_iter();
    assert!(!nodes.any(|n| n.rule == Rule::SingleStringLiteral));
}

#[test]
fn skip_literal_inside_other_literal() {
    fn test(c: &str) -> bool {
        emit_eq(c)
            && parse(c)
                .into_iter()
                .filter(|n| {
                    matches!(
                        n.rule,
                        Rule::DoubleStringLiteral
                            | Rule::SingleStringLiteral
                            | Rule::MultiLineCommentOpenBracket
                            | Rule::TemplateStringChars
                            | Rule::RegionOpen
                    )
                })
                .count()
                == 1
    }

    assert!(test("\"text 'not a lit'\""), "double qouted");
    assert!(test("'text \"not a lit\"'"), "single qouted");
    assert!(test("/* 'not a lit' */"), "multiline comment");
    assert!(test("/*\n'not a lit'\n*/"), "multiline comment \\n");
    assert!(test("`'not a lit'`"), "backtick");
    assert!(test("`\n'not a lit'\n`"), "backtick \\n");
    assert!(test("#text\n'not a lit'\n#endtext\n"), "text region");
    assert!(test("#sql\n'not a lit'\n#endsql\n"), "sql region");
}

#[test]
fn skip_single_line_commnet_inside_literal() {
    fn test(c: &str) -> bool {
        emit_eq(c)
            && parse(c)
                .into_iter()
                .all(|n| n.rule != Rule::SingleLineComment)
    }

    assert!(test("\"text // not a comment\""), "double qouted");
    assert!(test("/* // not a comment */"), "multiline comment");
    assert!(test("/* \n// not a comment \n*/"), "multiline comment \\n");
    assert!(test("`// not a comment`"), "backtick");
    assert!(test("`\n// not a comment\n`"), "backtick \\n");
    assert!(test("#text\n// not a comment\n#endtext\n"), "text region");
    assert!(test("#sql\n// not a comment\n#endsql\n"), "sql region");
}
