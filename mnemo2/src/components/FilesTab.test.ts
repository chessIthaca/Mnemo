import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  seedFilesPreview,
  type FileContent,
  type FileEntry,
  type FilesTransport,
} from "../lib/files";
import FilesTab from "./FilesTab.svelte";

const entry = (path: string, kind: FileEntry["kind"]): FileEntry => ({
  path,
  name: path.split("/").pop() ?? path,
  kind,
});

const TREE: Record<string, FileEntry[]> = {
  "": [entry("src", "dir"), entry("agent.md", "file")],
  src: [entry("src/main.rs", "file"), entry("src/notes.txt", "file")],
};

/** A transport over [`TREE`] whose reads and finds are spies the test inspects. */
function seed(tree = TREE) {
  const transport: FilesTransport = {
    list: vi.fn(async (path: string) => ({ path, entries: tree[path] ?? [], truncated: false })),
    read: vi.fn(
      async (path: string): Promise<FileContent> => ({
        path,
        text: path.endsWith(".rs") ? "fn main() {}\n" : "notes\n",
        bytes: 14,
        truncated: false,
        binary: false,
      }),
    ),
    find: vi.fn(async (query: string) =>
      Object.values(tree)
        .flat()
        .filter((candidate) => candidate.path.toLowerCase().includes(query.toLowerCase())),
    ),
  };
  seedFilesPreview(transport);
  return transport;
}

afterEach(() => {
  seedFilesPreview(null);
});

describe("FilesTab", () => {
  it("lists only the root, then one directory at a time", async () => {
    const transport = seed();
    render(FilesTab);

    const folder = await screen.findByRole("button", { name: "src" });
    expect(screen.getByRole("button", { name: "agent.md" })).toBeTruthy();
    // Nothing below the root has been asked for yet.
    expect(transport.list).toHaveBeenCalledTimes(1);
    expect(transport.list).toHaveBeenCalledWith("");

    await fireEvent.click(folder);

    expect(await screen.findByRole("button", { name: "main.rs" })).toBeTruthy();
    expect(transport.list).toHaveBeenCalledWith("src");
    expect(transport.list).toHaveBeenCalledTimes(2);
    // Collapsing keeps the listing: no second round-trip.
    await fireEvent.click(folder);
    expect(screen.queryByRole("button", { name: "main.rs" })).toBeNull();
    expect(transport.list).toHaveBeenCalledTimes(2);
  });

  it("opens a file read-only with its text highlighted", async () => {
    const transport = seed();
    render(FilesTab);

    await fireEvent.click(await screen.findByRole("button", { name: "src" }));
    await fireEvent.click(await screen.findByRole("button", { name: "main.rs" }));

    expect(transport.read).toHaveBeenCalledWith("src/main.rs");
    const viewer = await screen.findByLabelText("Contents of src/main.rs");
    expect(viewer.textContent).toContain("fn main()");
    expect(viewer.querySelector(".hljs-keyword")).not.toBeNull();

    // Back to the tree without losing what was already listed.
    await fireEvent.click(screen.getByRole("button", { name: "Back to the file tree" }));
    expect(screen.getByRole("button", { name: "main.rs" })).toBeTruthy();
  });

  it("shows the backend's refusal when a path leaves the project", async () => {
    const transport = seed();
    (transport.read as ReturnType<typeof vi.fn>).mockRejectedValue(
      "'../secrets.txt' is outside the project: '..' is not allowed",
    );
    render(FilesTab);

    await fireEvent.click(await screen.findByRole("button", { name: "agent.md" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("outside the project");
  });

  it("says so instead of showing content for a binary file", async () => {
    const transport = seed();
    (transport.read as ReturnType<typeof vi.fn>).mockResolvedValue({
      path: "agent.md",
      text: "",
      bytes: 2048,
      truncated: false,
      binary: true,
    });
    render(FilesTab);

    await fireEvent.click(await screen.findByRole("button", { name: "agent.md" }));
    expect(await screen.findByText("Binary file · the viewer shows text only.")).toBeTruthy();
    expect(screen.queryByLabelText("Contents of agent.md")).toBeNull();
  });

  it("filters project paths through the backend", async () => {
    const transport = seed();
    render(FilesTab);

    await fireEvent.input(screen.getByRole("textbox", { name: "Filter paths" }), {
      target: { value: "notes" },
    });

    await waitFor(() => expect(transport.find).toHaveBeenCalledWith("notes"));
    expect(await screen.findByRole("button", { name: "src/notes.txt" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "agent.md" })).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "Clear the path filter" }));
    expect(await screen.findByRole("button", { name: "agent.md" })).toBeTruthy();
  });
});
