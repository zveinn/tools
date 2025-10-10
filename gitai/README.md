# GitAI - GitHub Activity Monitor

A fast, colorful CLI tool for monitoring GitHub pull requests and issues across repositories. Track your contributions, reviews, and assignments with real-time progress visualization.

## Features

- 🚀 **Parallel API Calls** - Fetches data concurrently for maximum speed
- 🎨 **Colorized Output** - Easy-to-read color-coded labels, states, and progress
- 📊 **Smart Cross-Referencing** - Automatically links related PRs and issues
- ⚡ **Real-Time Progress Bar** - Visual feedback with color-coded completion status
- 🔍 **Comprehensive Search** - Tracks authored, mentioned, assigned, commented, and reviewed items
- 📅 **Time Filtering** - View open items from the last 6 months or closed items from the last month
- 🎯 **Organized Display** - Separates open, merged, and closed items into clear sections

## Installation

### Build from Source

```bash
go build -o gitai main.go
```

### Move to PATH (Optional)

```bash
sudo mv gitai /usr/local/bin/
```

## Configuration

### GitHub Token Setup

Create a GitHub Personal Access Token with the following scopes:
- `repo` - Access to repositories
- `read:org` - Read organization data

**Generate token:** https://github.com/settings/tokens

### Environment Setup

You can provide your token and username in three ways:

**Option 1: Environment Variables**
```bash
export GITHUB_TOKEN="your_token_here"
export GITHUB_USERNAME="your_username"
```

**Option 2: Configuration File**
Create `~/.secret/.gitai.env`:
```bash
GITHUB_TOKEN=your_token_here
GITHUB_USERNAME=your_username
```

## Usage

### Basic Usage

```bash
# Monitor open PRs and issues for the last 6 months
gitai

# Include closed items from last month
gitai --closed

# Show detailed logging output
gitai --debug
```

### Command Line Options

| Flag | Description |
|------|-------------|
| `--closed` | Include closed/merged PRs and issues from the last month |
| `--debug` | Show detailed API call progress instead of progress bar |

## Output Format

### Pull Requests Display

```
OPEN PULL REQUESTS:
------------------------------------------
2025/10/09 AUTHORED zveinn minio/madmin-go#462 - making sort non-case sensitive
-- OPEN miniohq/ec#87 - variable isCopied is always a nil

MERGED PULL REQUESTS:
------------------------------------------
2025/09/21 AUTHORED zveinn miniohq/ec#174 - isCopy is never set

CLOSED PULL REQUESTS:
------------------------------------------
2025/08/12 COMMENTED user helix-editor/helix#12204 - feat: new option
```

### Issues Display

```
OPEN ISSUES:
------------------------------------------
2025/10/10 AUTHORED zveinn tunnels-is/tunnels#140 - Error finding default route

CLOSED ISSUES:
------------------------------------------
2025/09/29 ASSIGNED user miniohq/eos#1688 - config: resolveconfigparam
```

### Color Coding

**Labels:**
- `AUTHORED` - Cyan
- `MENTIONED` - Yellow
- `ASSIGNED` - Magenta
- `COMMENTED` - Blue
- `REVIEWED` - Green
- `REVIEW REQUESTED` - Red
- `INVOLVED` - Gray

**States:**
- `OPEN` - Green
- `CLOSED` - Red
- `MERGED` - Magenta

**Usernames:** Each user gets a consistent color based on hash

## How It Works

1. **Parallel Fetching** - Simultaneously searches for:
   - PRs you authored
   - PRs where you're mentioned
   - PRs assigned to you
   - PRs you commented on
   - PRs you reviewed
   - PRs requesting your review
   - PRs involving you
   - Your recent activity events
   - Issues you authored/mentioned/assigned/commented

2. **Cross-Reference Detection** - Automatically finds connections between PRs and issues by:
   - Checking PR body and comments for issue references (`#123`, `fixes #123`, full URLs)
   - Checking issue body and comments for PR references
   - Displaying linked issues directly under their related PRs

3. **Smart Filtering**:
   - **Default mode**: Open items updated in last 6 months
   - **Closed mode** (`--closed`): Closed/merged items from last month

## API Rate Limits

GitAI monitors GitHub API rate limits and will warn you when running low:
- **Search API**: 30 requests per minute
- **Core API**: 5000 requests per hour

Rate limit status is displayed in debug mode.

## Troubleshooting

### "GITHUB_TOKEN environment variable is required"
Set up your GitHub token as described in [Configuration](#configuration).

### "Rate limit exceeded"
Wait for the rate limit to reset. Use `--debug` to see current rate limits.

### Progress bar looks garbled
Your terminal may not support ANSI colors properly. Use `--debug` mode for plain text output.

### No results showing
- Verify your username is correct
- Check that you have activity in the last 6 months
- Try with `--closed` to see closed items

## Development

### Project Structure
```
gitai/
├── main.go           # Main application code
├── README.md         # This file
└── .gitai.env        # Optional config file (in ~/.secret/)
```

## License

MIT License - Feel free to use and modify as needed.

