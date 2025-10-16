# PR Tool

A command-line tool for creating GitHub pull requests with optional AI-generated descriptions.

## Features

- Create draft pull requests between branches in the same repository
- Create pull requests from forks to parent repositories
- Automatically assign PRs to yourself
- Optional AI-generated PR descriptions using Claude CLI
- Simple command-line interface

## Prerequisites

- Go 1.16 or higher
- GitHub personal access token with `repo` scope
- (Optional) Claude CLI tool for AI-generated descriptions

## Installation

1. Clone this repository or download the source code

2. Build the tool:
```bash
go build -o pr-tool main.go
```

3. Create a `.env` file at `/home/sveinn/.github-feed/.env` with your GitHub credentials:
```
GITHUB_TOKEN=your_github_token_here
GITHUB_USERNAME=your_github_username
```

## Usage

### Basic syntax:
```bash
./pr-tool [source_repo/branch] [target_repo/branch] [msg] [--ai]
```

### Examples:

**Same repository PR:**
```bash
./pr-tool username/myrepo/feature-branch username/myrepo/main "Add new feature"
```

**Fork to parent repository PR:**
```bash
./pr-tool yourfork/repo/feature-branch upstream/repo/main "Add new feature"
```

**With AI-generated description:**
```bash
./pr-tool username/myrepo/feature-branch username/myrepo/main "Add new feature" --ai
```

## Parameters

- `source_repo/branch`: The source repository and branch (format: `owner/repo/branch`)
- `target_repo/branch`: The target repository and branch (format: `owner/repo/branch`)
- `msg`: The pull request title
- `--ai` (optional): Generate an AI-powered description using Claude CLI

## How it works

1. Parses source and target repository/branch information
2. Authenticates with GitHub using your personal access token
3. Creates a draft pull request
4. Assigns the PR to your GitHub username
5. (Optional) Generates and adds an AI description to the PR

## Notes

- All PRs are created as draft PRs by default
- The tool automatically enables "Allow edits from maintainers"
- For cross-repository PRs (forks), the tool automatically formats the head reference correctly
- AI descriptions require the Claude CLI tool to be installed and available in your PATH

## Troubleshooting

**"GITHUB_TOKEN not found in .env file"**
- Ensure your `.env` file exists at `/home/sveinn/.github-feed/.env`
- Verify the file contains `GITHUB_TOKEN=your_token`

**"Error creating pull request"**
- Verify your GitHub token has `repo` scope
- Ensure both branches exist
- Check that you have permission to create PRs in the target repository

**AI description generation fails**
- Verify Claude CLI is installed: `which claude`
- Check that the Claude CLI is properly configured
