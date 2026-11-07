#!/bin/bash
# SPDX-License-Identifier: MIT
# Source of Claude's secret-file guard. CodeSense embeds it (codesense_pty_core::notify_hook)
# and each host installs it as codesense-secret-guard.sh beside the status hook; a
# PreToolUse hook scoped to Read, Grep, Glob and Bash runs it. No workspace or team content
# carries a copy.
#
# It stops an agent reading a credential-shaped file into its transcript. The protected
# families come from one list in codesense_pty_core::notify_hook, which also generates Claude's
# permissions.deny rules: .env and .env.* (but not the .example/.sample/.template/.dist
# templates), *.pem, *.key, id_rsa*, id_ed25519* (but not their .pub halves), and anything
# under ~/.config/codesense-mcp/.
# A Bash command is checked for what it reads: a file that is only written, removed or the
# destination of a copy is not read. On a match it prints a PreToolUse deny with a
# reason the agent can pass on, and exits 0. Anything it cannot parse is allowed: the guard
# never blocks work because of its own failure.
exec /usr/bin/perl -MJSON::PP -e '
my $raw = do { local $/; <STDIN> };
my $d = eval { JSON::PP->new->decode($raw) } or exit 0;
my $tool = $d->{tool_name} // "";
my $in = $d->{tool_input};
exit 0 unless ref $in eq "HASH";

# Verbs whose operands are all written or removed, never read.
my %write_only = map { $_ => 1 } qw(rm rmdir unlink touch mkdir chmod chown chgrp ln truncate);
# Verbs whose last operand is the destination; the others are read.
my %to_destination = map { $_ => 1 } qw(cp mv install);
my %prefix = map { $_ => 1 } qw(sudo env command exec time nohup nice xargs);

# What the call READS: only those paths can put a credential into the transcript. A path that is
# only written, created, deleted or the destination of a copy is not read.
my @paths;
if ($tool eq "Read") { push @paths, $in->{file_path}; }
elsif ($tool eq "Grep") { push @paths, $in->{path}, $in->{glob}; }
elsif ($tool eq "Glob") { push @paths, $in->{pattern}, $in->{path}; }
elsif ($tool eq "Bash") { push @paths, bash_reads($in->{command} // ""); }
else { exit 0; }

sub bash_reads {
  my ($cmd) = @_;
  my @read;
  for my $segment (split /&&|\|\||;|\||\n/, $cmd) {
    (my $spaced = $segment) =~ s/(\d*[<>]+&?)/ $1 /g;
    my @words = grep { length } split /[\s"\x27`()]+/, $spaced;
    my (@operands, $verb);
    while (@words) {
      my $w = shift @words;
      if ($w =~ /^\d*>/) { shift @words; next; }          # a write target
      if ($w =~ /^\d*</) { push @read, shift @words; next; } # a redirected read
      next if !defined $verb && $w =~ /^[A-Za-z_]\w*=/;      # VAR=value before the verb
      if (!defined $verb) {
        (my $base = $w) =~ s{.*/}{};
        next if $prefix{$base};
        $verb = $base;
        next;
      }
      push @operands, $w;
    }
    next unless defined $verb;
    next if $write_only{$verb};
    if ($to_destination{$verb}) {
      my @args = grep { !/^-/ } @operands;
      pop @args;
      push @read, @args;
      next;
    }
    push @read, @operands;
  }
  # A value after = in an option (--env-file=.env) is an operand too.
  return map { my @p = ($_); push @p, $1 if /^-[^=]*=(.+)$/; @p } @read;
}

# @@PROTECTED@@  (replaced at install with @FAMILIES and @DIRS, generated from the one family list)

sub secret {
  my ($p) = @_;
  return 0 unless defined $p && length $p;
  for my $d (@DIRS) { return 1 if $p =~ m{(?:^|/)$d(?:/|$)}; }
  (my $base = $p) =~ s{.*/}{};
  # A glob names a family too: .env* and *.pem are checked as .env and .pem.
  for my $name ($base, $base =~ s/[*?\[\]{}]//gr) {
    next unless length $name;
    for my $family (@FAMILIES) {
      my ($re, $except) = @$family;
      next unless $name =~ $re;
      next if grep { $name =~ $_ } @$except;
      return 1;
    }
  }
  return 0;
}

for my $p (@paths) {
  next unless secret($p);
  my $reason = "CodeSense blocks reading credential files (.env, key files, ~/.config/codesense-mcp/) so secrets do not land in transcripts. Ask the user to give you the value another way, or read .env.example for the names.";
  print JSON::PP->new->canonical->encode({ hookSpecificOutput => {
    hookEventName => "PreToolUse", permissionDecision => "deny", permissionDecisionReason => $reason } });
  exit 0;
}
exit 0;
'
