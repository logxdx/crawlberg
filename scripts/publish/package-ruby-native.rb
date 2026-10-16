# frozen_string_literal: true

require "fileutils"
require "rbconfig"
require "rubygems/package"
require "stringio"
require "zlib"

module RubyNativePackage
  module_function

  def canonical_platform(platform)
    version = platform.version
    version = nil if platform.os == "darwin" || (platform.os == "linux" && version == "gnu")
    Gem::Platform.new([platform.cpu, platform.os, version]).to_s
  end

  def validate_inputs(directory, expected_platform, version)
    actual = canonical_platform(Gem::Platform.local)
    unless actual == expected_platform
      raise ArgumentError, "native platform #{actual} does not match #{expected_platform}"
    end

    spec = Gem::Specification.load(File.join(directory, "crawlberg.gemspec"))
    raise ArgumentError, "missing crawlberg gemspec" unless spec
    unless spec.name == "crawlberg" && spec.version.to_s == version
      raise ArgumentError, "gem identity/version does not match crawlberg #{version}"
    end

    library = File.join("lib", "crawlberg_rb.#{RbConfig::CONFIG.fetch("DLEXT")}")
    unless File.file?(File.join(directory, library)) && File.size(File.join(directory, library)).positive?
      raise ArgumentError, "missing or empty native extension #{library}"
    end

    [spec, library]
  end

  def native_spec(spec, library, platform)
    abi = RbConfig::CONFIG.fetch("ruby_version")
    major, minor = abi.split(".").map(&:to_i)
    destination = File.join("lib", "crawlberg_rb", abi, File.basename(library))
    FileUtils.mkdir_p(File.dirname(destination))
    FileUtils.cp(library, destination)
    native = spec.dup
    native.platform = Gem::Platform.new(platform)
    native.extensions = []
    native.required_ruby_version = [">= #{major}.#{minor}", "< #{major}.#{minor + 1}"]
    native.files = (spec.files + [destination]).uniq
    [native, destination]
  end

  def verify_package(path, platform, destination, original)
    package = Gem::Package.new(path)
    package.verify
    spec = package.spec
    major, minor = RbConfig::CONFIG.fetch("ruby_version").split(".").map(&:to_i)
    abi_range = Gem::Requirement.new(">= #{major}.#{minor}", "< #{major}.#{minor + 1}")
    unless spec.required_ruby_version == abi_range
      raise ArgumentError, "packaged native gem has invalid Ruby ABI requirement"
    end

    unless spec.platform.to_s == platform && spec.extensions.empty? && spec.files.include?(destination)
      raise ArgumentError, "packaged native gem has invalid platform or payload metadata"
    end

    payload = nil
    Gem::Package::TarReader.new(File.open(path, "rb")) do |outer|
      outer.each do |entry|
        next unless entry.full_name == "data.tar.gz"

        Zlib::GzipReader.wrap(StringIO.new(entry.read)) do |gzip|
          Gem::Package::TarReader.new(gzip) do |inner|
            inner.each { |file| payload = file.read if file.full_name == destination }
          end
        end
      end
    end

    raise ArgumentError, "packaged native extension bytes differ" unless payload == original
  end

  def run(arguments)
    unless arguments.length == 3
      raise ArgumentError, "usage: package-ruby-native.rb PACKAGE_DIR EXPECTED_PLATFORM VERSION"
    end

    directory, platform, version = arguments
    directory = File.expand_path(directory)
    Dir.chdir(directory) do
      spec, library = validate_inputs(directory, platform, version)
      original = File.binread(library)
      native, destination = native_spec(spec, library, platform)
      FileUtils.mkdir_p("pkg")
      filename = "crawlberg-#{version}-#{platform}.gem"
      result = Gem::Package.build(native, false, false, File.join("pkg", filename))
      raise ArgumentError, "native gem filename does not match #{filename}" unless File.basename(result) == filename

      verify_package(File.join("pkg", filename), platform, destination, original)
      puts("Verified native gem pkg/#{filename} (Ruby ABI #{RbConfig::CONFIG.fetch("ruby_version")})")
    end
  end
end

if $PROGRAM_NAME == __FILE__
  begin
    RubyNativePackage.run(ARGV)
  rescue ArgumentError => error
    warn(error.message)
    exit(1)
  end
end
