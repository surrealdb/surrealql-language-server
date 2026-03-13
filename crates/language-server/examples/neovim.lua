vim.filetype.add({
	extension = {
		surql = "surrealql",
		surrealql = "surrealql",
	},
})

local lspconfig = require("lspconfig")
local util = require("lspconfig.util")

lspconfig.surrealql_ls.setup({
	cmd = { "surreal-language-server" },
	filetypes = { "surrealql", "surql" },
	root_dir = util.root_pattern(".git"),
	single_file_support = true,
	settings = {
		surrealql = {
			connection = {
				endpoint = "ws://127.0.0.1:8000/rpc",
				namespace = "app",
				database = "app",
				username = "root",
				password = "root",
				token = vim.NIL,
				access = vim.NIL,
			},
			metadata = {
				mode = "workspace+db",
				enableLiveMetadata = true,
				refreshOnSave = true,
			},
			analysis = {
				enablePermissionAnalysis = true,
				enableAggressiveSchemaInference = true,
				enableCodeActions = true,
			},
			authContexts = {
				{
					name = "viewer",
					roles = { "viewer" },
					authRecord = "user:viewer",
					claims = {},
					session = {},
					variables = {},
				},
			},
			activeAuthContext = "viewer",
		},
	},
})
