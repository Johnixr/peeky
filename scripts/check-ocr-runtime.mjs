#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import os from "node:os";

const command = process.env.PEEKY_OCR_CMD || "paddleocr";

function log(line = "") {
  process.stdout.write(`${line}\n`);
}

function run(cmd, args) {
  return spawnSync(cmd, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

log("Peeky OCR runtime check");
log("========================");
log(`platform: ${process.platform} ${os.release()} ${os.arch()}`);
log(`node: ${process.version}`);
log(`ocr command: ${command}`);
log("");
log("Expected runtime:");
log("- PaddleOCR CLI must be available on PATH, or PEEKY_OCR_CMD must point to it.");
log("- The app calls: paddleocr ocr -i <selected-region.jpg> --lang ch --use_textline_orientation true");
log("- On a clean CI runner this usually fails until the OCR runtime is installed.");
log("");

const version = run(command, ["--version"]);
if (version.error) {
  log("Result: FAILED");
  log(`reason: cannot execute "${command}"`);
  log(`node error: ${version.error.message}`);
  log("");
  log("Install hints for CI:");
  log("  python -m pip install --upgrade pip");
  log("  python -m pip install paddlepaddle paddleocr");
  log("");
  log("Install hints for local macOS:");
  log("  python3 -m venv .venv-ocr");
  log("  source .venv-ocr/bin/activate");
  log("  pip install paddlepaddle paddleocr");
  log("");
  process.exit(1);
}

log("Command stdout:");
log(version.stdout.trim() || "(empty)");
log("");
log("Command stderr:");
log(version.stderr.trim() || "(empty)");

if (version.status !== 0) {
  log("");
  log(`Result: FAILED, "${command} --version" exited with ${version.status}`);
  process.exit(version.status || 1);
}

const ocrHelp = run(command, ["ocr", "--help"]);
if (ocrHelp.error) {
  log("");
  log("Result: FAILED");
  log(`reason: cannot execute "${command} ocr --help"`);
  log(`node error: ${ocrHelp.error.message}`);
  process.exit(1);
}

if (ocrHelp.status !== 0) {
  log("");
  log(`Result: FAILED, "${command} ocr --help" exited with ${ocrHelp.status}`);
  log("Command stderr:");
  log(ocrHelp.stderr.trim() || "(empty)");
  process.exit(ocrHelp.status || 1);
}

log("");
log("Result: OK");
