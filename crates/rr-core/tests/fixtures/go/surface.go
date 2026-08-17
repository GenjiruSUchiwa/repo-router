package app

// Server holds the listener.
type Server struct {
	Name string
}

// Runner is what a server satisfies.
type Runner interface {
	Run() error
}

const MaxRetries = 3

var defaultHost = "localhost"

// NewServer is the exported constructor.
func NewServer(name string) *Server {
	return &Server{Name: name}
}

func (s *Server) Run() error {
	return helper(s.Name)
}

func hidden() {}
