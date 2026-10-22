use super::*;
/// The grammar distinguishes comments from literal contents, including template
/// substitutions, raw strings, heredocs, and Python multiline strings.
pub(crate) fn comment_nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut comments = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "comment" | "line_comment" | "block_comment") {
            comments.push(node);
        } else {
            pending.extend(named_children(node).into_iter().rev());
        }
    }
    comments.sort_by_key(Node::start_byte);
    comments
}

pub(crate) fn count_lines_with_comments(source: &str, comments: &[Node<'_>]) -> Loc {
    let mut loc = Loc::default();
    let mut offset = 0;
    let mut comment_index = 0;
    for line in source.split_inclusive('\n') {
        loc.total += 1;
        let trimmed = if offset == 0 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };
        if trimmed.is_empty() {
            loc.blank += 1;
        } else {
            let mut has_code = false;
            for (column, ch) in line.char_indices() {
                if ch.is_whitespace() || (offset == 0 && column == 0 && ch == '\u{feff}') {
                    continue;
                }
                let byte = offset + column;
                while comments
                    .get(comment_index)
                    .is_some_and(|node| node.end_byte() <= byte)
                {
                    comment_index += 1;
                }
                if comments
                    .get(comment_index)
                    .is_none_or(|node| node.start_byte() > byte)
                {
                    has_code = true;
                    break;
                }
            }
            if has_code {
                loc.code += 1;
            } else {
                loc.comment += 1;
            }
        }
        offset += line.len();
    }
    loc
}
