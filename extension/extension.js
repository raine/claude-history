const vscode = require("vscode");

function activate(context) {
  // Register a command to list all available composer commands (for debugging)
  context.subscriptions.push(
    vscode.commands.registerCommand("mnemonai.listCommands", async () => {
      const allCommands = await vscode.commands.getCommands(true);
      const composerCommands = allCommands
        .filter((c) => c.toLowerCase().includes("composer"))
        .sort();
      const output = vscode.window.createOutputChannel("mnemonai");
      output.clear();
      output.appendLine("Available composer commands:");
      composerCommands.forEach((c) => output.appendLine(`  ${c}`));
      output.show();
    })
  );

  context.subscriptions.push(
    vscode.window.registerUriHandler({
      async handleUri(uri) {
        const params = new URLSearchParams(uri.query);
        const composerId = params.get("id");
        const debug = params.get("debug");

        if (debug === "commands") {
          await vscode.commands.executeCommand("mnemonai.listCommands");
          return;
        }

        if (!composerId) {
          vscode.window.showErrorMessage("mnemonai: Missing composer ID");
          return;
        }

        try {
          await vscode.commands.executeCommand(
            "composer.openComposer",
            composerId
          );
        } catch (err) {
          vscode.window.showErrorMessage(
            `mnemonai: Failed to open composer: ${err.message}`
          );
        }
      },
    })
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
