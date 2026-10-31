import { describe, expect, it } from "vitest";
import { ansiCss, parseAnsi, stripAnsi, terminalTail } from "./ansi";

const ESC = "\x1b";

describe("ansi", () => {
  it("turns SGR colors into styled segments", () => {
    const [line] = parseAnsi(`${ESC}[32m✓${ESC}[39m 12 passed`);
    expect(line.map((segment) => segment.text)).toEqual(["✓", " 12 passed"]);
    expect(line[0].style.color).toBeDefined();
    expect(line[1].style.color).toBeUndefined();
  });

  it("repairs an SGR whose escape byte was lost", () => {
    expect(stripAnsi("[32m✓[39m done")).toBe("✓ done");
    expect(parseAnsi("[1mbold[22m")[0][0].style.bold).toBe(true);
  });

  it("drops cursor, erase-line and OSC sequences", () => {
    expect(stripAnsi(`${ESC}[2K${ESC}[1Gbuilding${ESC}[?25l${ESC}]0;title\x07 ok`)).toBe("building ok");
  });

  it("keeps style across lines and resets on 0", () => {
    const lines = parseAnsi(`${ESC}[1;31merror\nstill red${ESC}[0m plain`);
    expect(lines[1][0].style).toMatchObject({ bold: true });
    expect(lines[1][1].style).toEqual({});
  });

  it("redraws a line on carriage return, even across a color change", () => {
    expect(stripAnsi("50%\r60%\r70%")).toBe("70%");
    expect(stripAnsi(`${ESC}[32m50%\r${ESC}[32m60%`)).toBe("60%");
    expect(stripAnsi("one\r\ntwo")).toBe("one\ntwo");
  });

  it("reads 256-color and truecolor codes", () => {
    expect(parseAnsi(`${ESC}[38;5;196mx`)[0][0].style.color).toBe("rgb(255 0 0)");
    expect(parseAnsi(`${ESC}[38;2;1;2;3mx`)[0][0].style.color).toBe("rgb(1 2 3)");
    expect(parseAnsi(`${ESC}[48;5;21mx`)[0][0].style.color).toBeUndefined();
  });

  it("tails the last lines without a trailing empty one", () => {
    const tail = terminalTail("a\nb\nc\nd\ne\nf\ng\n", 5);
    expect(tail.map((line) => line.map((segment) => segment.text).join(""))).toEqual(["c", "d", "e", "f", "g"]);
  });

  it("renders only the rules a style sets", () => {
    expect(ansiCss({})).toBe("");
    expect(ansiCss({ color: "#fff", bold: true })).toBe("color:#fff;font-weight:650");
  });
});
