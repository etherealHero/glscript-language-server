## Overview

Glscript-language-server is a language server that provides IDE functionality for writing glscript programs. You can use it with any editor that supports the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/) (VS Code, Vim, Emacs, Zed, etc).

After initialization glscript-language-server create proxy workspace for TSServer project with path `<project_dir>/.local/glproxy-workspace/`.

| Glproxy workspace files | Description                                      | Can edit/delete |
| ----------------------- | ------------------------------------------------ | --------------- |
| DEFAULT_INCLUDED.js     | includes for all scripts by default for TSServer | ✔️              |
| \_debug.emitted.js      | synced current client buffer for debug           | \*❌            |
| \_bundle.<hash>.js      | need for TSServer correct work                   | \*❌            |
| jsconfig.json           | copied from <project_dir> for TSServer project   | \*\*❌          |
| debug/\*_/_\*.\*        | emitted files in debug mode (for developers)     | ❌              |

\*❌ do not edit/delete this files until you work with project

\*\*❌ you need edit source jsconfig.json than restart glscript service (glscript rewrites jsconfig into glproxy workspace on init)

## Installation

### Prerequisites

1. [NodeJS](https://nodejs.org/en)
2. typescript & tsserver dependencies (version ^5 of tsserver not supported yet)
   ```sh
   npm i -D typescript typescript-language-server@4
   ```

### Setup

1. Download glscript-language-server and move to `<project_dir>/.local/glscript-language-server.exe`

<details>
<summary>Visual Studio Code</summary>

1. Install [Generic LSP Proxy](https://marketplace.visualstudio.com/items?itemName=mjmorales.generic-lsp-proxy) extension then add `<project_dir>/.vscode/lsp-proxy.json` config with same contents[[1]](https://github.com/mjmorales/vscode-generic-lsp-proxy?tab=readme-ov-file#example-configurations)[[2]](https://github.com/typescript-language-server/typescript-language-server/blob/master/docs/configuration.md#preferences-options):

   ```json
   [
     {
       "languageId": "glscript",
       "command": "%CD%/.local/glscript-language-server.exe",
       "args": ["./node_modules/.bin/typescript-language-server.cmd"],
       "fileExtensions": [".ts", ".js"],
       "initializationOptions": {
         "locale": "en"
       }
     }
   ]
   ```

2. Add `<project_dir>/.vscode/launch.json` for debugging[[1]](https://code.visualstudio.com/docs/debugtest/debugging)[[2]](https://code.visualstudio.com/docs/debugtest/debugging-configuration):

   ```json
   {
     "version": "0.2.0",
     "configurations": [
       {
         "name": "Debug NodeJS runtime",
         "type": "node",
         "request": "launch",
         "runtimeExecutable": "node",
         "runtimeArgs": ["--enable-source-maps", "--inspect-brk"],
         "program": "${workspaceFolder}/.local/glproxy-workspace/_debug.emitted.js"
       }
     ]
   }
   ```

3. Prefer disable builtin TypeScript extension. Both services built TypeScript & glscript-language-server can work improperly. Press <kbd>Ctrl+Shift+X</kbd>, type in search input "`@builtin TypeScript and JavaScript Language Features`" and disable builtin extension.
</details>

<details>
<summary>WebStorm</summary>

1. Install [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij) plugin.

2. Add new language server [(User guide)](https://github.com/redhat-developer/lsp4ij/blob/main/docs/UserDefinedLanguageServer.md):

   **Server**

   | Field   | Value                                                                                                              |
   | ------- | ------------------------------------------------------------------------------------------------------------------ |
   | Name    | glscript-language-server                                                                                           |
   | Command | `$PROJECT_DIR$/.local/glscript-language-server.exe $PROJECT_DIR$/node_modules/.bin/typescript-language-server.cmd` |

   **Mappings / File type**

   | File type  | Language Id |
   | ---------- | ----------- |
   | JavaScript | javascript  |
   | TypeScript | typescript  |

   **Mappings / File name patterns**

   | File name patterns | Language Id |
   | ------------------ | ----------- |
   | \*.js              | javascript  |
   | \*.ts              | typescript  |

   **Configuration / Server / Initialization Options[[1]](https://github.com/typescript-language-server/typescript-language-server/blob/master/docs/configuration.md#preferences-options)**

   ```json
   {
     "locale": "en"
   }
   ```

3. Prefer disable builtin JavaScript, TypeScript language support. Both services can work improperly.

</details>

## References

- [How We Made the Deno Language Server Ten Times Faster](https://denoland.medium.com/how-we-made-the-deno-language-server-ten-times-faster-62358af87d11)
- [How To Interpret Semantic Tokens](https://pygls.readthedocs.io/en/latest/protocol/howto/interpret-semantic-tokens.html)
- [Language server protocol overview](https://microsoft.github.io/language-server-protocol/overviews/lsp/overview/)
- [A Common Protocol for Languages](https://code.visualstudio.com/blogs/2016/06/27/common-language-protocol)
- [Debug your original code instead of deployed with source maps](https://developer.chrome.com/docs/devtools/javascript/source-maps)
- [Yet another explanation on sourcemap](https://medium.com/@trungutt/yet-another-explanation-on-sourcemap-669797e418ce)
- [Practical parsing with PEG and cpp-peglib](https://berthub.eu/articles/posts/practical-peg-parsing/)
- [PEGs and the Structure of Languages](https://blog.bruce-hill.com/pegs-and-the-structure-of-languages)
- [PEG Parsing Series Overview](https://medium.com/@gvanrossum_83706/peg-parsing-series-de5d41b2ed60)

## License

MIT
