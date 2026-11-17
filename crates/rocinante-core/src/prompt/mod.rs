//! System prompt assembly, written for a ~30B local model: short,
//! imperative, one instruction per sentence. Do not transplant frontier-model
//! prompts here — every token costs context and instruction-following.

use crate::config::Mode;

/// The mode paragraph of the system prompt. Normal and auto share one, so
/// switching between them leaves the prompt (and any prompt cache) as is;
/// plan mode has its own.
pub fn mode_block(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => {
            "You are in PLAN mode: analyze deeply, clarify, then plan. You may only read and search — do not edit files or run commands. Work in this exact order:\n\
             1. Restate the request in one sentence.\n\
             2. Investigate read-only until you understand every file and behavior the task touches. Think hard about the best way to do it. Anticipate the likely problems AND decide how you would solve each one before you propose anything.\n\
             3. List your assumptions and grey areas. If ANYTHING is still unclear — scope, edge cases, which of several approaches — ask the user UP TO 5 numbered clarifying questions ONCE, then STOP and wait. Do not ask again in later turns. Never plan around an ambiguity you could ask about.\n\
             4. Only when requirements are clear, present the final plan: numbered steps naming the exact files to change, the pitfalls you anticipate and how you will handle each, ending with how to verify the result.\n\
             5. Close with: \"Proceed with this plan? Switch to auto mode and say 'proceed' to run it hands-off.\""
        }
        Mode::Auto | Mode::Normal => {
            "Work step by step. Verify your changes by running commands (tests, builds) when possible."
        }
    }
}

pub fn system_prompt(cwd: &str, mode: Mode, os: &str) -> String {
    let mode_line = mode_block(mode);
    format!(
        r#"You are Rocinante, a coding agent working in a terminal.

Working directory: {cwd}
Operating system: {os}

{mode_line}

Rules:
- Use tools to inspect the project before making changes. Do not guess file contents.
- The bash tool already runs in the working directory. Never prefix commands with `cd`.
- Use relative paths.
- Before editing a file, read it first.
- To edit, copy the exact text from the file into old_string.
- When a command or edit fails, read the error and try a different approach.
- Keep your text responses short. Report what you did and what you found.
- When the task is done, summarize the outcome in one or two sentences.
- Questions about Rocinante itself (its commands, config, models, crew): load the rocinante-help skill and answer from its guide.

Discipline:
- Before a non-trivial change, state your assumption and approach in one sentence, then act.
- Write the minimum code that solves the problem. No speculative abstractions, options, or helpers for imagined future needs.
- Surgical edits only: never reformat, rename, or clean up code unrelated to the task.
- Prefer editing existing files over creating new ones. Never create documentation files unless asked.
- After changing code, verify it: run the project's build or tests before declaring the task done.
- Independent read-only lookups and task delegations may be issued together in one message; they run in parallel.

Tool call format: use the provided tools with valid JSON arguments. Never describe a tool call in prose — actually call the tool."#
    )
}

/// Start of the system-prompt line that states the critical-thinking mode.
pub const THINKING_PREFIX: &str = "Critical thinking (/thinking): ";

/// The system-prompt line stating whether critical thinking is on.
pub fn thinking_line(on: bool) -> String {
    if on {
        format!(
            "{THINKING_PREFIX}ON — a critic reviews your answers and finished work before the \
             user sees them (slower, checked). This is answer review, not model reasoning \
             (/think). If asked whether thinking mode is on, say it is on; /thinking turns it off."
        )
    } else {
        format!(
            "{THINKING_PREFIX}OFF — answers and work reach the user unreviewed (faster). This is \
             answer review, not model reasoning (/think). If asked whether thinking mode is on, \
             say it is off; /thinking turns it on."
        )
    }
}

/// Replace the line of `system` starting with `prefix` by `line`, or append
/// `line` after a blank line when there is none.
pub fn replace_prefixed_line(system: &str, prefix: &str, line: &str) -> String {
    let mut out = String::with_capacity(system.len() + line.len() + 2);
    let mut replaced = false;
    for (i, l) in system.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if !replaced && l.starts_with(prefix) {
            out.push_str(line);
            replaced = true;
        } else {
            out.push_str(l);
        }
    }
    if !replaced {
        out.push_str("\n\n");
        out.push_str(line);
    }
    out
}

/// Start of the system-prompt line present while `/talk` is on.
pub const VOICE_PREFIX: &str = "Voice (/talk): ";

/// A `/talk` conversation, with the speaker's name when one was given
/// (`/talk Donald`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VoiceChat {
    pub name: Option<String>,
    /// Replies are read aloud (Rocinante's speech is on).
    pub aloud: bool,
}

/// The system-prompt line for a `/talk` conversation.
pub fn voice_line(chat: &VoiceChat) -> String {
    let who = chat.name.as_deref().unwrap_or("the user");
    let address = match &chat.name {
        Some(name) => format!(" Address them as {name}."),
        None => String::new(),
    };
    let aloud = if chat.aloud {
        " Your replies are read aloud: answer in short, plain spoken sentences and do not \
         narrate your tool use. Use no markdown, lists or tables unless asked; code is not read \
         out, so put it in a file or on screen and say so."
    } else {
        ""
    };
    format!(
        "{VOICE_PREFIX}you are talking with {who} by voice. Their messages are speech-to-text \
         transcripts: expect misheard words, missing punctuation and spelled-out symbols \
         (\"main dot rs\" means main.rs). Read them for intent. If a request is unclear, or would \
         change or delete something and you are unsure what was said, ask one short question \
         first.{address}{aloud}"
    )
}

/// Put the voice line in `system` (right after its first line, so it
/// survives `/clear --all`), replacing any old one; `None` removes it.
pub fn set_voice_line(system: &str, chat: Option<&VoiceChat>) -> String {
    let mut lines: Vec<&str> = system
        .split('\n')
        .filter(|l| !l.starts_with(VOICE_PREFIX))
        .collect();
    let line = chat.map(voice_line);
    if let Some(line) = &line {
        lines.insert(1.min(lines.len()), line);
    }
    lines.join("\n")
}

/// System-prompt section for `.rocinante/PILOT.md` (project instructions).
pub fn pilot_section(content: &str) -> String {
    format!("\n\nProject instructions (from .rocinante/PILOT.md — follow these):\n{content}")
}

/// Start of the injected BRAINBOX section — the anchor `/clear --all` uses to
/// strip memory from a live system prompt. Kept as a prefix of the section
/// text so the two can't drift.
pub const BRAINBOX_SECTION_MARKER: &str = "\n\nProject memory (from previous sessions";

/// System-prompt section for `.rocinante/BRAINBOX.md` (session memory). Only
/// the compact head (goals + next steps) is injected; the model reads the
/// full file on demand, so standing context stays small.
pub fn brainbox_section(head: &str) -> String {
    format!(
        "{BRAINBOX_SECTION_MARKER} — may be stale; verify before relying on it):\n{head}\n\nFull project memory is in .rocinante/BRAINBOX.md — read it with the `read` tool when you need earlier decisions, state, or gotchas."
    )
}

/// Start of the injected global-rules section.
pub const LESSONS_SECTION_MARKER: &str = "\n\nGlobal user rules (from ~/.rocinante/LESSONS.md";

/// System-prompt section for `~/.rocinante/LESSONS.md` — the user's global
/// preferences and do/don't rules, injected in full (it's small and the agent
/// must follow it every turn). Sits before the BRAINBOX section, so a
/// project's `/clear --all` leaves it intact.
pub fn lessons_section(content: &str) -> String {
    format!("{LESSONS_SECTION_MARKER} — always follow these):\n{content}")
}

/// The canned task submitted by `/remember <text>`: record a global user
/// preference/rule in `~/.rocinante/LESSONS.md`.
pub fn remember_prompt(rule: &str) -> String {
    let path = dirs::home_dir()
        .map(|h| h.join(".rocinante/LESSONS.md").display().to_string())
        .unwrap_or_else(|| "~/.rocinante/LESSONS.md".to_string());
    format!(
        "Record a global preference for this user in {path} (use this exact absolute path). \
         The rule to record is: {rule}\n\n\
         Read the file first if it exists; create it with the headings `# LESSONS`, \
         `## Preferences`, and `## Rules` if not. Decide whether this is a Preference (a \
         taste or default) or a Rule (a hard do/don't) and append ONE concise imperative \
         bullet under the right heading. If a near-equivalent bullet already exists, refine \
         it in place instead of duplicating. Edit surgically — never reformat or remove \
         unrelated lines. Re-read the file afterward to confirm it is well-formed. Then tell \
         the user in one line what you recorded and where."
    )
}

/// The canned task submitted by `/commit`.
pub fn commit_prompt() -> &'static str {
    "Run `git status` and `git diff` (and `git diff --staged`) to see all pending changes. Group them into one atomic commit — or say why they should be several, and do only the first. Stage exactly the files that belong together (never `git add -A` blindly, never include unrelated files), then commit with a concise imperative message that says what changed and why. Report the commit hash and message."
}

/// The canned task submitted by `/config`. Embeds the absolute user-config
/// path because the file tools do not expand `~`.
pub fn config_prompt(request: &str) -> String {
    let path = dirs::home_dir()
        .map(|h| h.join(".rocinante/config.toml").display().to_string())
        .unwrap_or_else(|| "~/.rocinante/config.toml".to_string());
    let opening = "Load the rocinante-config skill first — call the `skill` tool with {\"name\": \"rocinante-config\"} and follow it exactly.";
    if request.is_empty() {
        format!(
            "{opening} Then read the Rocinante user config file at {path} (use this exact absolute path). \
             If it does not exist, say so and list what can be configured. Otherwise summarize each section \
             present in plain language: which model aliases exist (with their num_ctx), which providers, \
             subagent profiles, permission rules, and MCP/LSP servers. Do not edit anything."
        )
    } else {
        format!(
            "{opening} Then update the Rocinante user config file at {path} (use this exact absolute path) \
             to satisfy this request: {request}. Read the file first if it exists; create it if not. \
             Edit surgically — never remove or reformat unrelated sections. Re-read the file after editing \
             to verify the TOML is valid. When done, tell the user: new model aliases appear in /model \
             immediately; other config sections apply on the next launch."
        )
    }
}

/// The canned task submitted by `/init`.
pub fn init_prompt() -> &'static str {
    "Explore this project: read the README if present, the build manifests, the directory layout, and skim key source files. Then write the file .rocinante/PILOT.md — a concise guide for an AI coding agent working here. Use exactly these sections: a 2-3 sentence description of what the project is; build/test/run commands; an architecture map (main directories/modules and their roles); project conventions worth knowing. Keep it under 60 lines. If .rocinante/PILOT.md already exists, read it first and update it rather than rewriting from scratch."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_line_is_replaced_not_duplicated() {
        let sys = "You are Rocinante.";
        let on = replace_prefixed_line(sys, THINKING_PREFIX, &thinking_line(true));
        assert!(on.ends_with(&thinking_line(true)), "{on}");
        let off = replace_prefixed_line(&on, THINKING_PREFIX, &thinking_line(false));
        assert_eq!(off.matches(THINKING_PREFIX).count(), 1, "{off}");
        assert!(off.contains("OFF") && !off.contains("ON —"), "{off}");
        assert!(off.starts_with("You are Rocinante."));
    }

    #[test]
    fn remember_prompt_and_lessons_section() {
        let p = remember_prompt("always run cargo fmt before committing");
        assert!(p.contains(".rocinante/LESSONS.md") && p.contains("LESSONS.md"));
        assert!(p.contains("always run cargo fmt"));
        assert!(p.contains("Preference") && p.contains("Rule"));
        if dirs::home_dir().is_some() {
            assert!(!p.contains("~/"), "must embed the absolute path, not ~");
        }
        let s = lessons_section("## Rules\n- x");
        assert!(s.starts_with(LESSONS_SECTION_MARKER));
        assert!(s.contains("- x"));
    }

    #[test]
    fn brainbox_section_points_to_file() {
        let s = brainbox_section("## Goals\n- ship it");
        assert!(s.contains("- ship it"));
        assert!(s.contains(".rocinante/BRAINBOX.md"));
        assert!(
            s.contains("read"),
            "must tell the model to read the full file"
        );
    }

    #[test]
    fn config_prompt_embeds_path_skill_and_request() {
        let p = config_prompt("add alias kimiko for kimi-k2.7-code:cloud with num_ctx 256000");
        assert!(
            p.contains("{\"name\": \"rocinante-config\"}"),
            "skill JSON missing"
        );
        assert!(
            p.contains(".rocinante") && p.contains("config.toml"),
            "path missing"
        );
        assert!(p.contains("add alias kimiko"), "request text missing");
        if dirs::home_dir().is_some() {
            assert!(!p.contains("~/"), "must embed the absolute path, not ~");
        }
    }

    #[test]
    fn plan_mode_asks_up_to_five_questions_once() {
        let p = system_prompt("/tmp", Mode::Plan, "macos");
        assert!(p.contains("UP TO 5"), "plan mode caps questions at 5");
        assert!(p.contains("ONCE"), "plan mode asks only once");
        assert!(
            p.to_ascii_lowercase().contains("anticipate"),
            "plan mode anticipates issues"
        );
    }

    #[test]
    fn bare_config_prompt_is_read_only() {
        let p = config_prompt("");
        assert!(p.contains("summarize"), "bare /config should summarize");
        assert!(p.contains("Do not edit"), "bare /config must be read-only");
        assert!(
            !p.contains("update the Rocinante user config"),
            "bare /config must not edit"
        );
    }

    #[test]
    fn voice_line_goes_after_the_first_line_and_comes_out_cleanly() {
        let base = system_prompt("/tmp", Mode::Normal, "linux");
        let donald = VoiceChat {
            name: Some("Donald".into()),
            aloud: false,
        };
        let on = set_voice_line(&base, Some(&donald));
        let mut lines = on.lines();
        assert!(lines.next().unwrap().starts_with("You are Rocinante"));
        let voice = lines.next().unwrap();
        assert!(voice.starts_with(VOICE_PREFIX));
        assert!(voice.contains("talking with Donald by voice"));
        assert!(voice.contains("Address them as Donald."));
        // Renaming replaces the line instead of adding a second one.
        let renamed = set_voice_line(&on, Some(&VoiceChat::default()));
        assert_eq!(renamed.matches(VOICE_PREFIX).count(), 1);
        assert!(renamed.contains("talking with the user by voice"));
        assert!(!renamed.contains("Address them"));
        assert!(!voice.contains("read aloud"));
        let aloud = set_voice_line(
            &on,
            Some(&VoiceChat {
                name: Some("Donald".into()),
                aloud: true,
            }),
        );
        assert!(aloud.contains("Your replies are read aloud"));
        assert_eq!(aloud.matches(VOICE_PREFIX).count(), 1);
        assert_eq!(
            aloud.lines().filter(|l| l.contains("read aloud")).count(),
            1
        );
        // Off restores the original prompt byte for byte.
        assert_eq!(set_voice_line(&renamed, None), base);
        // And survives `/clear --all`, which cuts at the memory marker.
        let with_memory = format!("{on}{}", brainbox_section("notes"));
        let cut = &with_memory[..with_memory.find(BRAINBOX_SECTION_MARKER).unwrap()];
        assert!(cut.contains(VOICE_PREFIX));
    }
}
