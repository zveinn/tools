package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/google/go-github/v57/github"
	"github.com/joho/godotenv"
	"golang.org/x/oauth2"
)

type RepoBranch struct {
	Owner  string
	Repo   string
	Branch string
}

func main() {
	if len(os.Args) < 4 {
		fmt.Fprintf(os.Stderr, "Usage: %s [source_repo/branch] [target_repo/branch] [msg] [--ai]\n", os.Args[0])
		fmt.Fprintf(os.Stderr, "Example: %s zveinn/myrepo/feature-branch zveinn/myrepo/main \"Add new feature\"\n", os.Args[0])
		fmt.Fprintf(os.Stderr, "Example with AI: %s zveinn/myrepo/feature-branch zveinn/myrepo/main \"Add new feature\" --ai\n", os.Args[0])
		os.Exit(1)
	}

	sourceArg := os.Args[1]
	targetArg := os.Args[2]
	prTitle := os.Args[3]

	// Check for --ai flag
	useAI := false
	if len(os.Args) > 4 && os.Args[4] == "--ai" {
		useAI = true
	}

	// Load .env file from /home/sveinn/.github-feed
	envPath := "/home/sveinn/.github-feed/.env"
	err := godotenv.Load(envPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error loading .env file from %s: %v\n", envPath, err)
		os.Exit(1)
	}

	githubToken := os.Getenv("GITHUB_TOKEN")
	githubUsername := os.Getenv("GITHUB_USERNAME")

	if githubToken == "" {
		fmt.Fprintf(os.Stderr, "GITHUB_TOKEN not found in .env file\n")
		os.Exit(1)
	}

	if githubUsername == "" {
		fmt.Fprintf(os.Stderr, "GITHUB_USERNAME not found in .env file\n")
		os.Exit(1)
	}

	// Parse source and target
	source, err := parseRepoBranch(sourceArg)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error parsing source: %v\n", err)
		os.Exit(1)
	}

	target, err := parseRepoBranch(targetArg)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error parsing target: %v\n", err)
		os.Exit(1)
	}

	// Create GitHub client
	ctx := context.Background()
	ts := oauth2.StaticTokenSource(
		&oauth2.Token{AccessToken: githubToken},
	)
	tc := oauth2.NewClient(ctx, ts)
	client := github.NewClient(tc)

	// Create the pull request
	draft := true

	// For cross-repository PRs (fork to parent), use owner:branch format for Head
	headRef := source.Branch
	if source.Owner != target.Owner || source.Repo != target.Repo {
		headRef = fmt.Sprintf("%s:%s", source.Owner, source.Branch)
	}

	newPR := &github.NewPullRequest{
		Title:               github.String(prTitle),
		Head:                github.String(headRef),
		Base:                github.String(target.Branch),
		Draft:               &draft,
		MaintainerCanModify: github.Bool(true),
	}

	pr, _, err := client.PullRequests.Create(ctx, target.Owner, target.Repo, newPR)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error creating pull request: %v\n", err)
		os.Exit(1)
	}

	fmt.Printf(" Created draft PR #%d: %s\n", pr.GetNumber(), pr.GetHTMLURL())

	// Assign the PR to the GitHub username
	_, _, err = client.Issues.AddAssignees(ctx, target.Owner, target.Repo, pr.GetNumber(), []string{githubUsername})
	if err != nil {
		fmt.Fprintf(os.Stderr, "Warning: Failed to assign PR to %s: %v\n", githubUsername, err)
	} else {
		fmt.Printf(" Assigned PR to %s\n", githubUsername)
	}

	// If --ai flag is set, generate and update PR description
	if useAI {
		fmt.Println(" Generating AI description for PR...")
		aiDescription, err := generateAIDescription(source, target, prTitle)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: Failed to generate AI description: %v\n", err)
		} else {
			// Update the PR with the AI-generated description
			updatePR := &github.PullRequest{
				Body: github.String(aiDescription),
			}
			_, _, err = client.PullRequests.Edit(ctx, target.Owner, target.Repo, pr.GetNumber(), updatePR)
			if err != nil {
				fmt.Fprintf(os.Stderr, "Warning: Failed to update PR with AI description: %v\n", err)
			} else {
				fmt.Printf(" Updated PR with AI-generated description\n")
			}
		}
	}
}

func generateAIDescription(source, target *RepoBranch, prTitle string) (string, error) {
	// Build the prompt for Claude
	prompt := fmt.Sprintf(
		"Generate a concise and professional pull request description for the following PR:\n\n"+
			"Title: %s\n"+
			"Source branch: %s\n"+
			"Target branch: %s\n\n"+
			"The description should include:\n"+
			"- A brief summary of changes\n"+
			"- Key improvements or features\n"+
			"- Any relevant context\n\n"+
			"Keep it professional and under 300 words. Once the description is complete, apply the approriate labels",
		prTitle, source.Branch, target.Branch,
	)

	// Call claude CLI tool
	cmd := exec.Command("claude", "-p", prompt)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("failed to execute claude CLI: %w (output: %s)", err, string(output))
	}

	return strings.TrimSpace(string(output)), nil
}

func parseRepoBranch(arg string) (*RepoBranch, error) {
	// Expected format: owner/repo/branch
	parts := strings.SplitN(arg, "/", 3)
	if len(parts) != 3 {
		return nil, fmt.Errorf("invalid format '%s', expected owner/repo/branch", arg)
	}

	return &RepoBranch{
		Owner:  parts[0],
		Repo:   parts[1],
		Branch: parts[2],
	}, nil
}
