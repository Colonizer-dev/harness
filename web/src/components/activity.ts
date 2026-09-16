/**
 * One plain sentence per tool call, for the chat's simple view.
 *
 * Agents don't send a description with their commands, so the wording is derived from the call itself.
 * Anything unrecognised falls back to naming the command rather than guessing at its intent.
 */

export type ActivityIcon = "run" | "read" | "edit" | "search" | "download" | "web" | "memory" | "test" | "git" | "clean" | "list" | "agent";

export interface Activity {
  label: string;
  icon: ActivityIcon;
}

const str = (value: unknown): string => (typeof value === "string" ? value : "");

const baseName = (path: string): string => path.split("/").filter(Boolean).pop() ?? path;

const quote = (text: string, max = 48): string => {
  const clean = text.replace(/\s+/g, " ").replace(/^['"]|['"]$/g, "").trim();
  return clean.length > max ? `“${clean.slice(0, max)}…”` : `“${clean}”`;
};

const hostOf = (url: string): string => {
  try {
    return new URL(url).host;
  } catch {
    return "";
  }
};

/** Shell no-ops and bare echoes say nothing to a reader watching the colony work. */
export function isNoiseTool(toolName: string, input: Record<string, unknown>): boolean {
  if (toolName !== "Bash") return false;
  const command = str(input.command).trim();
  return command === "" || command === "true" || command === ":" || /^echo\b/.test(command);
}

/** The part of a shell line that does the work, past `cd …`, `mkdir …` and env assignments. */
function mainCommand(command: string): string {
  const parts = command
    .split(/&&|\|\||\||;/)
    .map((part) => part.trim())
    .filter(Boolean);
  const meaningful = parts.find((part) => !/^(cd|export|set|mkdir|echo|true|:)\b/.test(part));
  return (meaningful ?? parts[0] ?? "").replace(/^(\w+=\S+\s+)+/, "");
}

function describeBash(command: string): Activity {
  const main = mainCommand(command);
  const tool = main.split(/\s+/)[0] ?? "";
  const has = (pattern: RegExp) => pattern.test(main);
  switch (tool) {
    case "git":
      return has(/^git\s+(status|diff|log|show)\b/)
        ? { icon: "git", label: "Checking what changed" }
        : { icon: "git", label: "Working with git" };
    case "curl":
    case "wget": {
      const host = hostOf(main.match(/https?:\/\/[^\s"']+/)?.[0] ?? "");
      return { icon: "download", label: host ? `Downloading from ${host}` : "Downloading a file" };
    }
    case "df":
    case "du":
      return { icon: "run", label: "Checking free disk space" };
    case "rm":
      return { icon: "clean", label: "Cleaning up files" };
    case "cp":
    case "mv":
      return { icon: "edit", label: "Moving files around" };
    case "chmod":
    case "chown":
      return { icon: "edit", label: "Adjusting file permissions" };
    case "ls":
    case "tree":
      return { icon: "list", label: "Listing files" };
    case "find":
    case "fd":
      return { icon: "search", label: "Looking for files" };
    case "grep":
    case "rg": {
      const term = main.match(/(?:grep|rg)\s+(?:-\S+\s+)*("[^"]+"|'[^']+'|\S+)/)?.[1];
      return { icon: "search", label: term ? `Searching the code for ${quote(term)}` : "Searching the code" };
    }
    case "cat":
    case "head":
    case "tail":
    case "sed":
      return { icon: "read", label: "Reading a file" };
    case "tar":
    case "unzip":
      return { icon: "download", label: "Unpacking an archive" };
    case "npm":
    case "pnpm":
    case "yarn":
      if (has(/\b(install|ci|add)\b/)) return { icon: "download", label: "Installing dependencies" };
      if (has(/\btest\b/)) return { icon: "test", label: "Running the tests" };
      if (has(/\bbuild\b/)) return { icon: "run", label: "Building the web interface" };
      return { icon: "run", label: "Running a package script" };
    case "cargo":
      if (has(/\btest\b/)) return { icon: "test", label: "Running the Rust tests" };
      if (has(/\bclippy\b/)) return { icon: "test", label: "Checking the Rust code for problems" };
      return { icon: "run", label: "Building the Rust code" };
    case "swift":
      return has(/\btest\b/) ? { icon: "test", label: "Running the Swift tests" } : { icon: "run", label: "Building the Swift package" };
    case "xcodebuild":
      return { icon: "run", label: "Building the app" };
    case "xcrun":
      return has(/swift-format/) ? { icon: "edit", label: "Checking code formatting" } : { icon: "run", label: "Running an Xcode tool" };
    case "node":
    case "python":
    case "python3":
      return has(/--test|pytest|unittest/) ? { icon: "test", label: "Running the tests" } : { icon: "run", label: "Running a script" };
    case "apt":
    case "apt-get":
    case "brew":
      return { icon: "download", label: "Installing a tool" };
    default:
      return { icon: "run", label: tool ? `Running ${tool}` : "Running a command" };
  }
}

export function describeTool(toolName: string, input: Record<string, unknown>): Activity {
  const file = str(input.file_path) || str(input.path) || str(input.notebook_path);
  switch (toolName) {
    case "Bash":
      return describeBash(str(input.command));
    case "Read":
      return { icon: "read", label: file ? `Reading ${baseName(file)}` : "Reading a file" };
    case "Edit":
    case "MultiEdit":
    case "NotebookEdit":
      return { icon: "edit", label: file ? `Editing ${baseName(file)}` : "Editing a file" };
    case "Write":
      return { icon: "edit", label: file ? `Writing ${baseName(file)}` : "Writing a file" };
    case "Glob":
      return { icon: "search", label: "Looking for files" };
    case "Grep": {
      const pattern = str(input.pattern);
      return { icon: "search", label: pattern ? `Searching the code for ${quote(pattern)}` : "Searching the code" };
    }
    case "WebFetch": {
      const host = hostOf(str(input.url));
      return { icon: "web", label: host ? `Reading a page on ${host}` : "Reading a web page" };
    }
    case "WebSearch": {
      const query = str(input.query);
      return { icon: "web", label: query ? `Searching the web for ${quote(query)}` : "Searching the web" };
    }
    case "Task":
    case "Agent":
      return { icon: "agent", label: "Asking a helper agent" };
    case "TodoWrite":
      return { icon: "list", label: "Updating its plan" };
    default:
      if (toolName.includes("memory_search")) return { icon: "memory", label: "Looking through shared memory" };
      if (toolName.includes("memory_propose")) return { icon: "memory", label: "Proposing a note for shared memory" };
      if (toolName.startsWith("mcp__")) return { icon: "run", label: "Using a connected tool" };
      return { icon: "run", label: toolName };
  }
}
