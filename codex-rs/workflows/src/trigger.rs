#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowTrigger {
    Ultracode { objective: String },
    PendingUltracode,
    DeepResearch { topic: String },
    PendingDeepResearch,
}

pub fn parse_workflow_trigger(input: &str) -> Option<WorkflowTrigger> {
    let trimmed = input.trim();

    if let Some(remainder) = strip_command(trimmed, "/effort") {
        let remainder = remainder.trim_start();
        if let Some(objective) = strip_keyword_prefix(remainder, "ultracode") {
            let objective = objective.trim();
            return if objective.is_empty() {
                Some(WorkflowTrigger::PendingUltracode)
            } else {
                Some(WorkflowTrigger::Ultracode {
                    objective: objective.to_string(),
                })
            };
        }
    }

    if let Some(topic) = strip_command(trimmed, "/deep-research") {
        let topic = topic.trim();
        return if topic.is_empty() {
            Some(WorkflowTrigger::PendingDeepResearch)
        } else {
            Some(WorkflowTrigger::DeepResearch {
                topic: topic.to_string(),
            })
        };
    }

    let range = find_unfenced_keyword(input, "ultracode")?;
    Some(WorkflowTrigger::Ultracode {
        objective: remove_trigger(input, range),
    })
}

fn strip_command<'a>(input: &'a str, command: &str) -> Option<&'a str> {
    let prefix = input.get(..command.len())?;
    if !prefix.eq_ignore_ascii_case(command) {
        return None;
    }

    let remainder = &input[command.len()..];
    if remainder.is_empty() || remainder.starts_with(char::is_whitespace) {
        Some(remainder)
    } else {
        None
    }
}

fn strip_keyword_prefix<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let prefix = input.get(..keyword.len())?;
    if !prefix.eq_ignore_ascii_case(keyword) {
        return None;
    }

    let remainder = &input[keyword.len()..];
    if remainder.is_empty() || remainder.starts_with(char::is_whitespace) {
        Some(remainder)
    } else {
        None
    }
}

fn find_unfenced_keyword(input: &str, keyword: &str) -> Option<std::ops::Range<usize>> {
    let mut offset = 0;
    let mut fence: Option<Fence> = None;

    for segment in input.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let marker = fence_marker(trimmed);

        if let Some(active_fence) = fence {
            if marker.is_some_and(|marker| {
                marker.character == active_fence.character
                    && marker.length >= active_fence.length
                    && marker.closes(trimmed)
            }) {
                fence = None;
            }
        } else if let Some(marker) = marker {
            fence = Some(marker);
        } else if let Some(range) = find_keyword(line, keyword) {
            return Some(offset + range.start..offset + range.end);
        }

        offset += segment.len();
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence {
    character: char,
    length: usize,
}

impl Fence {
    fn closes(self, line: &str) -> bool {
        line[self.length..].trim().is_empty()
    }
}

fn fence_marker(line: &str) -> Option<Fence> {
    let character = line.chars().next()?;
    if !matches!(character, '`' | '~') {
        return None;
    }
    let length = line
        .chars()
        .take_while(|candidate| *candidate == character)
        .count();
    (length >= 3).then_some(Fence { character, length })
}

fn find_keyword(input: &str, keyword: &str) -> Option<std::ops::Range<usize>> {
    for (start, _) in input.char_indices() {
        let end = start.checked_add(keyword.len())?;
        let Some(candidate) = input.get(start..end) else {
            continue;
        };
        if candidate.eq_ignore_ascii_case(keyword)
            && is_word_boundary(input[..start].chars().next_back())
            && is_word_boundary(input[end..].chars().next())
        {
            return Some(start..end);
        }
    }

    None
}

fn is_word_boundary(character: Option<char>) -> bool {
    character.is_none_or(|character| !is_word_character(character))
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || unicode_ident::is_xid_continue(character)
}

fn remove_trigger(input: &str, range: std::ops::Range<usize>) -> String {
    let mut start = range.start;
    let mut end = range.end;

    while start > 0
        && input[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        start -= input[..start].chars().next_back().map_or(0, char::len_utf8);
    }
    if start > 0
        && input[..start]
            .chars()
            .next_back()
            .is_some_and(|character| matches!(character, ',' | ';' | ':'))
    {
        start -= 1;
    } else {
        while end < input.len()
            && input[end..]
                .chars()
                .next()
                .is_some_and(|character| matches!(character, ',' | ';' | ':'))
        {
            end += 1;
        }
        while end < input.len() && input[end..].chars().next().is_some_and(char::is_whitespace) {
            end += input[end..].chars().next().map_or(0, char::len_utf8);
        }
    }

    format!("{}{}", &input[..start], &input[end..])
        .trim()
        .to_string()
}
