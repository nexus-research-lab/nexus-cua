#!/usr/bin/env ruby
# frozen_string_literal: true

require "pathname"
require "uri"

ROOT = Pathname.new(__dir__).join("..").expand_path
Dir.chdir(ROOT)
files = (`git ls-files '*.md'`.lines +
  `git ls-files --others --exclude-standard '*.md'`.lines).map(&:strip).uniq
failures = []

files.each do |relative|
  file = ROOT.join(relative)
  text = file.read
  links = text.scan(/!?\[[^\]]*\]\(([^)]+)\)/).flatten
  links.concat(text.scan(/^\s*\[[^\]]+\]:\s*(\S+)/).flatten)
  links.each do |raw|
    target = raw.strip.sub(/\s+["'][^"']*["']\z/, "").delete_prefix("<").delete_suffix(">")
    next if target.empty? || target.start_with?("http:", "https:", "mailto:", "tel:", "data:")

    path_part, anchor = target.split("#", 2)
    destination = if path_part.empty?
      file
    else
      decoded = URI::DEFAULT_PARSER.unescape(path_part.split("?", 2).first)
      file.dirname.join(decoded).cleanpath
    end
    unless destination.exist?
      failures << "#{relative}: missing #{target}"
      next
    end
    next if anchor.nil? || anchor.empty? || !destination.file? || destination.extname.downcase != ".md"

    slugs = Hash.new(0)
    anchors = destination.read.lines.each_with_object([]) do |line, values|
      heading = line[/^\s{0,3}[#]{1,6}\s+(.+?)\s*#*\s*$/, 1]
      next unless heading

      base = heading.downcase
        .gsub(/<[^>]+>/, "")
        .gsub(/[^\p{L}\p{N}\s_-]/, "")
        .strip
        .gsub(/[\s_]+/, "-")
        .gsub(/-+/, "-")
      count = slugs[base]
      slugs[base] += 1
      values << (count.zero? ? base : "#{base}-#{count}")
    end
    decoded_anchor = URI::DEFAULT_PARSER.unescape(anchor).downcase
    unless anchors.include?(decoded_anchor)
      failures << "#{relative}: missing anchor ##{anchor} in #{destination.relative_path_from(ROOT)}"
    end
  end
end

abort(failures.join("\n")) unless failures.empty?
puts "Markdown links valid: #{files.length} files"
