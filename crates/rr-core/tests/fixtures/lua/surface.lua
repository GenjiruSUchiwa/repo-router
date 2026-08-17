local M = {}
Service = {}

function bare(x)
  return x
end

function M.run(x)
  return helper(x)
end

function Service:start()
  return self
end

local function hidden()
  return 1
end

function Service.new()
  return {}
end
