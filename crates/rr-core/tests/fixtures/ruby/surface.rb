# Documented service.
class Service < Base
  ATTEMPTS = 3

  def initialize(name)
    @name = name
  end

  def run
    helper(@name)
  end

  def _hidden
    1
  end

  private

  def secret
    2
  end
end

module Util
  def self.build
    Service.new('x')
  end
end
