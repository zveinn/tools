# GitAI - GitHub PR Activity Monitor

A CLI tool that monitors all GitHub pull request activity for a specified user.

## Features

Monitors and displays:
- ✏️  PRs created by the user
- 💬 PRs where the user is mentioned
- 👤 PRs where the user is assigned
- 👁️  PRs reviewed by the user
- 🔔 PRs where review is requested from the user
- 🏠 PRs on repositories owned by the user

## Installation

```bash
go build -o gitai
```

## Setup

### Generate a GitHub Personal Access Token

1. Go to https://github.com/settings/tokens
2. Click "Generate new token" → "Generate new token (classic)"
3. Give it a descriptive name (e.g., "GitAI CLI")
4. Select these scopes:
   - `repo` (Full control of private repositories)
   - `read:org` (Read org and team membership)
5. Click "Generate token"
6. Copy the token (you won't be able to see it again!)

### Set Environment Variables

```bash
export GITHUB_TOKEN=your_token_here
export GITHUB_USER=your_github_username  # Optional if passing as argument
```

## Usage

```bash
# Using command line argument
./gitai <github_username>

# Using environment variable
export GITHUB_USER=your_username
./gitai
```

The tool will:
1. Display all current PR activity for the specified user
2. Refresh automatically every 2 minutes
3. Use colors to highlight different types of activity
4. Show one line per PR for minimal, clean output

Press `Ctrl+C` to stop monitoring.

## Output Format

```
● 🔔 Review Requested username owner/repo#123 - PR title here
◐ ✏️  Created username owner/repo#456 - Draft PR title
● 💬 Mentioned username owner/repo#789 - Another PR
```

- ● Green dot = Open PR
- ◐ Yellow half-circle = Draft PR
- Colored labels indicate activity type
- Format: `status icon label author owner/repo#number - title`
