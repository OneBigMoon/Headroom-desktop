import { describe, expect, it } from "vitest";
import { extensionCopy, extensionErrorKey } from "./extensionManager";

describe("extension manager copy and safe errors", () => {
  it("provides all labels for each existing application locale", () => {
    for (const copy of Object.values(extensionCopy)) {
      expect(Object.keys(copy).sort()).toEqual(Object.keys(extensionCopy.en).sort());
      expect(Object.values(copy).every((label) => label.trim().length > 0)).toBe(true);
    }
  });
  it("maps conflicts without rendering the raw server error", () => {
    expect(extensionErrorKey("server already exists")).toBe("conflict");
    expect(extensionErrorKey(new Error("destination conflict"))).toBe("conflict");
    expect(extensionErrorKey("目标技能已存在；不会覆盖：/fixture/existing-skill")).toBe("conflict");
    expect(extensionErrorKey("技能导入需要非空的 name 和 description；原技能和已有文件未修改。")).toBe("invalidSkill");
    expect(extensionErrorKey("Invalid MCP JSON. Use a mcpServers object with unique keys and valid field types.")).toBe("invalidJson");
    expect(extensionErrorKey("an arbitrary provider response")).toBe("failed");
  });
});
