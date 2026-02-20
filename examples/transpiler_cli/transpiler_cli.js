/**
 * Demonstrates how to programmatically use the glscript-language-server proxy via CLI.
 * This example connects to the LSP server, requests a custom "Transpile to ES syntax" code action,
 * and outputs the resulting transpiled code to stdout.
 */
const path = require('path'),
  fs = require('fs'),
  { pathToFileURL } = require('url'),
  cp = require('child_process'),
  rpc = require('vscode-jsonrpc'),
  lsp = require('vscode-languageserver-protocol')

if (!process.argv[2]) return console.error('Usage: node transpiler_cli.js <file.js>')

const target = path.resolve(process.cwd(), process.argv[2])
const cwd = path.dirname(target)
const glserver = path.join(__dirname, '..', '..', 'target', 'release', 'glscript-language-server.exe')

if (!fs.existsSync(glserver)) return console.error('Server not found:', glserver)

const proc = cp.spawn(glserver, [path.join(__dirname, 'node_modules', '.bin', 'typescript-language-server.cmd')], {
  stdio: ['pipe', 'pipe', 'pipe'],
  cwd
})
const connection = rpc.createMessageConnection(
  new rpc.StreamMessageReader(proc.stdout),
  new rpc.StreamMessageWriter(proc.stdin)
)

connection.listen()
;(async () => {
  try {
    const uri = pathToFileURL(target).href
    const text = fs.readFileSync(target, 'utf-8')
    await connection.sendRequest('initialize', {
      processId: process.pid,
      capabilities: {
        textDocument: { codeAction: { codeActionLiteralSupport: { codeActionKind: { valueSet: [''] } } } },
        workspace: { applyEdit: true, workspaceEdit: { documentChanges: true } }
      },
      workspaceFolders: [{ uri: pathToFileURL(cwd).href, name: 'root' }]
    })
    connection.sendNotification('initialized', {})
    connection.sendNotification('textDocument/didOpen', {
      textDocument: { uri, languageId: 'javascript', version: 1, text }
    })

    // await new Promise(r => setTimeout(r, 1000)) // Wait for server indexing

    const actions = await connection.sendRequest('textDocument/codeAction', {
      textDocument: { uri },
      range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } },
      context: { diagnostics: [] }
    })
    const action = (actions || []).find(a => lsp.CodeAction.is(a) && a.title.includes('Transpile to ES syntax'))
    if (!action) return console.error('Action not found. Available:', (actions || []).map(a => a.title).join(', '))

    let workspaceEdit = action.edit
    if (!workspaceEdit && action.command) {
      const res = await connection.sendRequest('workspace/executeCommand', action.command)
      if (res && (res.documentChanges || res.changes)) workspaceEdit = res
    }

    if (workspaceEdit) {
      const changes = workspaceEdit.documentChanges
        ? workspaceEdit.documentChanges.find(c => c.textDocument.uri === uri)
        : { edits: workspaceEdit.changes[uri] }
      console.log(changes ? changes.edits[0].newText : 'No changes for this file')
      /*
        expected:
        import   "some/script.js"

        var message =
            `Hello world
        `;

        message;
      */
    } else console.error('Server returned no WorkspaceEdit')
  } catch (e) {
    console.error('Error:', e.message)
  } finally {
    try {
      await connection.sendRequest('shutdown')
      connection.sendNotification('exit')
    } catch {}
    proc.kill()
  }
})()
