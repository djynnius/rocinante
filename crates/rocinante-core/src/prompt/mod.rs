//! System prompt assembly, written for a ~30B local model: short,
//! imperative, one instruction per sentence. Do not transplant frontier-model
//! prompts here — every token costs context and instruction-following.

use crate::config::Mode;

pub fn system_prompt(cwd: &str, mode: Mode, os: &str) -> String {
    let mode_line = match mode {
        Mode::Plan => {
            "You are in PLAN mode: analyze deeply, clarify, then plan. You may only read and search — do not edit files or run commands. Work in this exact order:\n\
             1. Restate the request in one sentence.\n\
             2. Investigate read-only until you understand every file and behavior the task touches.\n\
             3. List your assumptions and grey areas. If ANYTHING is ambiguous — scope, edge cases, which of several approaches — ask the user numbered clarifying questions and STOP. Wait for answers. Never plan around an ambiguity you could ask about.\n\
             4. Only when requirements are clear, present the final plan: numbered steps naming the exact files to change, ending with how to verify the result.\n\
             5. Close with: \"Proceed with this plan? Switch to auto mode and say 'proceed' to run it hands-off.\""
        }
        Mode::Auto | Mode::Normal => {
            "Work step by step. Verify your changes by running commands (tests, builds) when possible."
        }
    };
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
    fn bare_config_prompt_is_read_only() {
        let p = config_prompt("");
        assert!(p.contains("summarize"), "bare /config should summarize");
        assert!(p.contains("Do not edit"), "bare /config must be read-only");
        assert!(
            !p.contains("update the Rocinante user config"),
            "bare /config must not edit"
        );
    }
}
