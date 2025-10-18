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

	// For cross-repository PRs (fork to parent), use owner:branch format for Head
	headRef := source.Branch
	if source.Owner != target.Owner || source.Repo != target.Repo {
		headRef = fmt.Sprintf("%s:%s", source.Owner, source.Branch)
	}

	// Check if PR already exists
	existingPR, err := findExistingPR(ctx, client, target.Owner, target.Repo, headRef, target.Branch)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error checking for existing PR: %v\n", err)
		os.Exit(1)
	}

	if existingPR != nil {
		fmt.Printf(" PR already exists: #%d: %s\n", existingPR.GetNumber(), existingPR.GetHTMLURL())

		// If --ai flag is set, only update the description
		if useAI {
			fmt.Println(" Generating AI description for existing PR...")
			err := generateAIDescriptionInteractive(existingPR.GetHTMLURL(), prTitle, source, target)
			if err != nil {
				fmt.Fprintf(os.Stderr, "Warning: Failed to generate AI description: %v\n", err)
			} else {
				fmt.Printf(" Claude is updating the PR description...\n")
			}
		} else {
			fmt.Println(" No action taken (use --ai flag to update description)")
		}
		return
	}

	// Create the pull request
	draft := true

	newPR := &github.NewPullRequest{
		Title:               github.String(prTitle),
		Head:                github.String(headRef),
		Base:                github.String(target.Branch),
		Draft:               &draft,
		MaintainerCanModify: github.Bool(true),
	}
	fmt.Println(newPR)

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
		err := generateAIDescriptionInteractive(pr.GetHTMLURL(), prTitle, source, target)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: Failed to generate AI description: %v\n", err)
		} else {
			fmt.Printf(" Claude is updating the PR description...\n")
		}
	}
}

func findExistingPR(ctx context.Context, client *github.Client, owner, repo, head, base string) (*github.PullRequest, error) {
	// List all open pull requests for the base branch
	// We'll filter manually because GitHub's Head filter can be inconsistent
	opts := &github.PullRequestListOptions{
		State: "open",
		Base:  base,
		ListOptions: github.ListOptions{
			PerPage: 100,
		},
	}

	prs, _, err := client.PullRequests.List(ctx, owner, repo, opts)
	if err != nil {
		return nil, err
	}

	// Manually filter for matching head branch
	// Handle both "branch" and "owner:branch" formats
	for _, pr := range prs {
		prHead := pr.GetHead()
		if prHead == nil {
			continue
		}

		// Build the PR's head reference in the same format we're searching for
		prHeadRef := prHead.GetRef()
		if prHead.GetRepo() != nil && prHead.GetRepo().GetOwner() != nil {
			prHeadOwner := prHead.GetRepo().GetOwner().GetLogin()
			// If it's a cross-repo PR, use owner:branch format
			if prHeadOwner != owner {
				prHeadRef = fmt.Sprintf("%s:%s", prHeadOwner, prHead.GetRef())
			}
		}

		// Check if this PR matches our head reference
		if prHeadRef == head {
			return pr, nil
		}
	}

	return nil, nil
}

func generateAIDescriptionInteractive(prURL, prTitle string, source, target *RepoBranch) error {
	// Build the prompt for Claude in interactive mode
	prompt := fmt.Sprintf(
		"Please use the gh CLI tool to update the description for the GitHub PR at %s\n\n"+
			"PR Title: %s\n"+
			"Source branch: %s\n"+
			"Target branch: %s\n\n"+
			"Generate a concise and professional pull request description that includes:\n"+
			"- A brief summary of changes\n"+
			"- Key improvements or features\n"+
			"- Any relevant context\n\n"+
			"Keep it professional and under 300 words. Use the github cli tool (gh) to update the PR description with the new description. Once you are done editing the description, please apply the appropriate labels from the already existing labels (do no make new ones).",
		prURL, prTitle, source.Branch, target.Branch,
	)

	// Call claude CLI tool with the prompt, allowing it to use tools
	cmd := exec.Command("claude", prompt)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr

	err := cmd.Run()
	if err != nil {
		return fmt.Errorf("failed to execute claude CLI: %w", err)
	}

	return nil
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
