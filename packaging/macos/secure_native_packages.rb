#!/usr/bin/env ruby
# frozen_string_literal: true

gem 'native-packages', '= 0.10.0'
require 'native_packages'
require 'native_packages/macos_signing'
require_relative 'credential_transport'
NativePackages::MacosSigning.prepend(SpotifastMacosCredentialTransport)
NativePackages::CLI.run(Dir.pwd, ARGV)
